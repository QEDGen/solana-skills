import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { Escrow } from "../target/types/escrow";
import { PublicKey, Keypair, SystemProgram } from "@solana/web3.js";
import {
  TOKEN_PROGRAM_ID,
  createMint,
  createAccount,
  mintTo,
  getAccount,
} from "@solana/spl-token";
import { assert } from "chai";

describe("escrow", () => {
  // Configure the client to use the local cluster.
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.Escrow as Program<Escrow>;

  let mintA: PublicKey;
  let mintB: PublicKey;
  let initializerTokenAccountA: PublicKey;
  let initializerTokenAccountB: PublicKey;
  let takerTokenAccountA: PublicKey;
  let takerTokenAccountB: PublicKey;

  let payer: Keypair;
  let initializer: PublicKey;
  const taker = Keypair.generate();

  const initializerAmount = 1000;
  const takerAmount = 500;

  before(async () => {
    // Create a test keypair and fund it
    payer = Keypair.generate();
    const airdropSig = await provider.connection.requestAirdrop(
      payer.publicKey,
      10 * anchor.web3.LAMPORTS_PER_SOL
    );
    await provider.connection.confirmTransaction(airdropSig);
    initializer = payer.publicKey;

    // Airdrop SOL to taker
    const signature = await provider.connection.requestAirdrop(
      taker.publicKey,
      2 * anchor.web3.LAMPORTS_PER_SOL
    );
    await provider.connection.confirmTransaction(signature);

    // Create mints
    mintA = await createMint(
      provider.connection,
      payer,
      initializer,
      null,
      6
    );

    mintB = await createMint(
      provider.connection,
      payer,
      initializer,
      null,
      6
    );

    // Create token accounts for initializer
    initializerTokenAccountA = await createAccount(
      provider.connection,
      payer,
      mintA,
      initializer
    );

    initializerTokenAccountB = await createAccount(
      provider.connection,
      payer,
      mintB,
      initializer
    );

    // Create token accounts for taker
    takerTokenAccountA = await createAccount(
      provider.connection,
      payer,
      mintA,
      taker.publicKey
    );

    takerTokenAccountB = await createAccount(
      provider.connection,
      payer,
      mintB,
      taker.publicKey
    );

    // Mint tokens to initializer and taker
    await mintTo(
      provider.connection,
      payer,
      mintA,
      initializerTokenAccountA,
      initializer,
      initializerAmount
    );

    await mintTo(
      provider.connection,
      payer,
      mintB,
      takerTokenAccountB,
      initializer,
      takerAmount
    );
  });

  it("Initialize escrow", async () => {
    const [escrowPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("escrow"), initializer.toBuffer()],
      program.programId
    );

    const [escrowTokenAccount] = PublicKey.findProgramAddressSync(
      [Buffer.from("escrow_token"), initializer.toBuffer()],
      program.programId
    );

    await program.methods
      .initialize(
        new anchor.BN(initializerAmount),
        new anchor.BN(takerAmount)
      )
      .accounts({
        initializer: initializer,
        escrow: escrowPda,
        initializerDepositTokenAccount: initializerTokenAccountA,
        mint: mintA,
        escrowTokenAccount: escrowTokenAccount,
        systemProgram: SystemProgram.programId,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .signers([payer])
      .rpc();

    // Verify escrow state
    const escrowAccount = await program.account.escrowState.fetch(escrowPda);
    assert.ok(escrowAccount.initializer.equals(initializer));
    assert.ok(
      escrowAccount.initializerTokenAccount.equals(initializerTokenAccountA)
    );
    assert.strictEqual(
      escrowAccount.initializerAmount.toNumber(),
      initializerAmount
    );
    assert.strictEqual(escrowAccount.takerAmount.toNumber(), takerAmount);

    // Verify tokens were transferred to escrow
    const escrowTokenAccountData = await getAccount(
      provider.connection,
      escrowTokenAccount
    );
    assert.strictEqual(
      Number(escrowTokenAccountData.amount),
      initializerAmount
    );

    // Verify initializer's token account was debited
    const initializerTokenAccountData = await getAccount(
      provider.connection,
      initializerTokenAccountA
    );
    assert.strictEqual(Number(initializerTokenAccountData.amount), 0);
  });

  it("Exchange escrow", async () => {
    const [escrowPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("escrow"), initializer.toBuffer()],
      program.programId
    );

    const [escrowTokenAccount] = PublicKey.findProgramAddressSync(
      [Buffer.from("escrow_token"), initializer.toBuffer()],
      program.programId
    );

    await program.methods
      .exchange()
      .accounts({
        taker: taker.publicKey,
        escrow: escrowPda,
        takerDepositTokenAccount: takerTokenAccountB,
        takerReceiveTokenAccount: takerTokenAccountA,
        initializerReceiveTokenAccount: initializerTokenAccountB,
        escrowTokenAccount: escrowTokenAccount,
        initializer: initializer,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .signers([taker])
      .rpc();

    // Verify taker received initializer's tokens
    const takerTokenAccountAData = await getAccount(
      provider.connection,
      takerTokenAccountA
    );
    assert.strictEqual(
      Number(takerTokenAccountAData.amount),
      initializerAmount
    );

    // Verify initializer received taker's tokens
    const initializerTokenAccountBData = await getAccount(
      provider.connection,
      initializerTokenAccountB
    );
    assert.strictEqual(Number(initializerTokenAccountBData.amount), takerAmount);

    // Verify taker's deposit account was debited
    const takerTokenAccountBData = await getAccount(
      provider.connection,
      takerTokenAccountB
    );
    assert.strictEqual(Number(takerTokenAccountBData.amount), 0);

    // Verify escrow account was closed
    try {
      await program.account.escrowState.fetch(escrowPda);
      assert.fail("Escrow account should be closed");
    } catch (err) {
      assert.ok(err.message.includes("Account does not exist"));
    }
  });

  it("Initialize and cancel escrow", async () => {
    // Use a different payer for this test to avoid PDA collision
    const newPayer = Keypair.generate();
    const airdropSig = await provider.connection.requestAirdrop(
      newPayer.publicKey,
      10 * anchor.web3.LAMPORTS_PER_SOL
    );
    await provider.connection.confirmTransaction(airdropSig);

    // Create new token account for this payer
    const newInitializerTokenAccount = await createAccount(
      provider.connection,
      newPayer,
      mintA,
      newPayer.publicKey
    );

    // Mint tokens to new account (using original payer as mint authority)
    await mintTo(
      provider.connection,
      payer,
      mintA,
      newInitializerTokenAccount,
      initializer,
      initializerAmount
    );

    const [escrowPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("escrow"), newPayer.publicKey.toBuffer()],
      program.programId
    );

    const [escrowTokenAccount] = PublicKey.findProgramAddressSync(
      [Buffer.from("escrow_token"), newPayer.publicKey.toBuffer()],
      program.programId
    );

    // Initialize new escrow
    await program.methods
      .initialize(
        new anchor.BN(initializerAmount),
        new anchor.BN(takerAmount)
      )
      .accounts({
        initializer: newPayer.publicKey,
        escrow: escrowPda,
        initializerDepositTokenAccount: newInitializerTokenAccount,
        mint: mintA,
        escrowTokenAccount: escrowTokenAccount,
        systemProgram: SystemProgram.programId,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .signers([newPayer])
      .rpc();

    // Cancel the escrow
    await program.methods
      .cancel()
      .accounts({
        initializer: newPayer.publicKey,
        escrow: escrowPda,
        initializerDepositTokenAccount: newInitializerTokenAccount,
        escrowTokenAccount: escrowTokenAccount,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .signers([newPayer])
      .rpc();

    // Verify tokens were returned to initializer
    const initializerTokenAccountData = await getAccount(
      provider.connection,
      newInitializerTokenAccount
    );
    assert.strictEqual(
      Number(initializerTokenAccountData.amount),
      initializerAmount
    );

    // Verify escrow account was closed
    try {
      await program.account.escrowState.fetch(escrowPda);
      assert.fail("Escrow account should be closed");
    } catch (err) {
      assert.ok(err.message.includes("Account does not exist"));
    }
  });
});
