import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { Multisig } from "../target/types/multisig";

const { PublicKey, Keypair, SystemProgram } = anchor.web3;

import {
  TOKEN_PROGRAM_ID,
  createTransferInstruction,
  getAssociatedTokenAddressSync,
  createInitializeMintInstruction,
  createMintToInstruction,
  createAssociatedTokenAccountInstruction,
  createSetAuthorityInstruction,
  AuthorityType,
  MINT_SIZE,
  getMinimumBalanceForRentExemptMint,
} from "@solana/spl-token";

describe("multisig with SPL Token", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.multisig as Program<Multisig>;

  // 成员
  const member1 = Keypair.generate();
  const member2 = Keypair.generate();
  const member3 = Keypair.generate();
  const members = [member1, member2, member3];
  const memberPubkeys = members.map((m) => m.publicKey);

  // 接收者
  const receiver = Keypair.generate();

  // 测试用 Token
  const mint = Keypair.generate(); // 我们自己当 mint authority

  let multisigPda: PublicKey;
  const nonce = 0;

  // Token 账户
  let vaultAta: PublicKey; // 实际由 member1 创建，但 owner 是 multisigPda
  let receiverAta: PublicKey;

  before(async () => {
    // 空投 SOL
    for (const member of members) {
      await provider.connection.confirmTransaction(
        await provider.connection.requestAirdrop(member.publicKey, 2 * anchor.web3.LAMPORTS_PER_SOL),
        "confirmed"
      );
    }
    await provider.connection.confirmTransaction(
      await provider.connection.requestAirdrop(receiver.publicKey, 1 * anchor.web3.LAMPORTS_PER_SOL),
      "confirmed"
    );

    // 创建 Mint 账户
    const rent = await getMinimumBalanceForRentExemptMint(provider.connection);
    const createMintTx = new anchor.web3.Transaction().add(
      SystemProgram.createAccount({
        fromPubkey: member1.publicKey,
        newAccountPubkey: mint.publicKey,
        space: MINT_SIZE,
        lamports: rent,
        programId: TOKEN_PROGRAM_ID,
      }),
      createInitializeMintInstruction(
        mint.publicKey,
        6, // decimals
        member1.publicKey, // mint authority
        null // freeze authority
      )
    );
    await provider.sendAndConfirm(createMintTx, [member1, mint]);

    console.log("✅ Mint created:", mint.publicKey.toString());
  });

  it("Creates a multisig", async () => {
    [multisigPda] = PublicKey.findProgramAddressSync(
      [
        Buffer.from("multisig"),
        member1.publicKey.toBuffer(),
        Buffer.from([nonce]),
      ],
      program.programId
    );

    const tx = await program.methods
      .createMultisig(nonce, memberPubkeys, 2)
      .accounts({
        multisig: multisigPda,
        creator: member1.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .signers([member1])
      .rpc();

    console.log("✅ Multisig created:", multisigPda.toString());
  });

  it("Creates token accounts and transfers ownership to multisig", async () => {
    // 1. member1 创建自己的 ATA（作为金库）
    const member1Ata = getAssociatedTokenAddressSync(mint.publicKey, member1.publicKey);
    vaultAta = member1Ata; // 之后 owner 会改为 multisigPda

    // 2. receiver 的 ATA
    receiverAta = getAssociatedTokenAddressSync(mint.publicKey, receiver.publicKey);

    // 指令
    const createVaultAtaIx = createAssociatedTokenAccountInstruction(
      member1.publicKey,
      member1Ata,
      member1.publicKey,
      mint.publicKey
    );

    const createReceiverAtaIx = createAssociatedTokenAccountInstruction(
      member1.publicKey,
      receiverAta,
      receiver.publicKey,
      mint.publicKey
    );

    const mintToIx = createMintToInstruction(
      mint.publicKey,
      member1Ata,
      member1.publicKey,
      10_000n
    );

    // 关键：将 vaultAta 的 owner 从 member1 改为 multisigPda
    const setAuthorityIx = createSetAuthorityInstruction(
      member1Ata,
      member1.publicKey, // current owner
      AuthorityType.AccountOwner,
      multisigPda // new owner
    );

    const tx = new anchor.web3.Transaction()
      .add(createVaultAtaIx)
      .add(createReceiverAtaIx)
      .add(mintToIx)
      .add(setAuthorityIx);

    await provider.sendAndConfirm(tx, [member1]);

    console.log("✅ Vault ATA created, minted, and ownership transferred to multisig");
  });

  let proposalPda: PublicKey;

  it("Proposes a token transfer transaction", async () => {
    // 从 vaultAta 转账（owner = multisigPda）
    const transferIx = createTransferInstruction(
      vaultAta,
      receiverAta,
      multisigPda, // owner
      1000n
    );

    console.log("Transfer IX accounts:");
    transferIx.keys.forEach((k, i) => {
      console.log(i, k.pubkey.toBase58(), "writable:", k.isWritable, "signer:", k.isSigner);
    });

    // 添加 TOKEN_PROGRAM_ID 到 accounts（CPI 需要）
    const fullAccounts = [
      ...transferIx.keys,
      { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
    ];

    const instructionData = {
      programId: TOKEN_PROGRAM_ID,
      accounts: fullAccounts.map((k) => ({
        pubkey: k.pubkey,
        isSigner: k.isSigner,
        isWritable: k.isWritable,
      })),
      data: Buffer.from(transferIx.data)
    };

    // Proposal PDA (index = 0)
    [proposalPda] = PublicKey.findProgramAddressSync(
      [
        Buffer.from("proposal"),
        multisigPda.toBuffer(),
        new anchor.BN(0).toBuffer("le", 4),
      ],
      program.programId
    );

    const tx = await program.methods
      .proposeTransaction(instructionData)
      .accounts({
        multisig: multisigPda,
        proposal: proposalPda,
        proposer: member1.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .signers([member1])
      .rpc();

    console.log("✅ Proposal created:", tx);
  });

  it("Approves the proposal by member2", async () => {
    const tx = await program.methods
      .approveTransaction()
      .accounts({
        multisig: multisigPda,
        proposal: proposalPda,
        approver: member2.publicKey,
      })
      .signers([member2])
      .rpc();

    console.log("✅ Approved by member2:", tx);
  });

  it("Approves the proposal by member3", async () => {
    const tx = await program.methods
      .approveTransaction()
      .accounts({
        multisig: multisigPda,
        proposal: proposalPda,
        approver: member3.publicKey,
      })
      .signers([member3])
      .rpc();

    console.log("✅ Approved by member3:", tx);
  });

  it("Executes the token transfer proposal", async () => {
    const balanceBefore = await provider.connection.getTokenAccountBalance(receiverAta);
    console.log("Receiver balance before:", balanceBefore.value.uiAmount);

    const proposalAccount = await program.account.proposal.fetch(proposalPda);

  
    // 构造 remainingAccounts：multisigPda 的 isSigner 设为 false
    const remainingAccounts = proposalAccount.instruction.accounts.map((acc: any) => {
      const pubkey = new PublicKey(acc.pubkey);
      let isWritable = acc.isWritable;
      let isSigner = acc.isSigner;

      if (pubkey.equals(multisigPda)) {
        isSigner = false;   // 必须 false（避免签名缺失）
        isWritable = false; // ⚠️ 必须 false！与提案一致
      }

      console.log(pubkey.toBase58(), "writable:", acc.isWritable, "signer:", acc.isSigner);
      return {
        pubkey,
        isSigner,
        isWritable,
      };
    });
    

    const txSig = await program.methods
      .executeTransaction()
      .accounts({
        multisig: multisigPda,
        proposal: proposalPda,
      })
      .remainingAccounts(remainingAccounts)
      .rpc();

    console.log("✅ Proposal executed:", txSig);

    await provider.connection.confirmTransaction(txSig, "confirmed");

    // 👇 手动获取交易日志
    const tx = await provider.connection.getTransaction(txSig, {
      commitment: "confirmed",
    });
    console.log("Transaction logs:");
    console.log(tx.meta.logMessages.join("\n"));

    const balanceAfter = await provider.connection.getTokenAccountBalance(receiverAta);
    console.log("Receiver balance after:", balanceAfter.value.uiAmount);
    console.log("Transferred amount:", (balanceAfter.value.uiAmount || 0) - (balanceBefore.value.uiAmount || 0));
  });
});