# Shielded Token Architecture

Technical documentation for developers working with ZSwap shielded tokens in the Midnight ledger.

## Table of Contents

1. [Overview](#1-overview)
2. [Core Data Structures](#2-core-data-structures)
3. [Key Derivation](#3-key-derivation)
4. [Commitments and Nullifiers](#4-commitments-and-nullifiers)
5. [ZSwap State](#5-zswap-state)
6. [ZSwap Offer Components](#6-zswap-offer-components)
7. [Value Commitments and Balancing](#7-value-commitments-and-balancing)
8. [Proof System](#8-proof-system)
9. [Contract Interactions](#9-contract-interactions)
10. [Transaction Flow](#10-transaction-flow)
11. [Coin Derivation](#11-coin-derivation)

---

## 1. Overview

ZSwap is a Zerocash-like privacy system where coins are never stored directly. Instead, the ledger maintains:

- **Commitment set**: Hashed representations of coins (reveals nothing about value/owner)
- **Nullifier set**: Markers indicating a coin has been spent (cannot be linked to commitments)

The set of unspent coins is conceptually `commitment_set - nullifier_set`, but this difference is **not computable** without knowledge of secret keys.

Reference: [spec/zswap.md](../spec/zswap.md)

---

## 2. Core Data Structures

### 2.1 Coin Information

A shielded coin consists of three fields ([coin-structure/src/coin.rs#L580-L587](../coin-structure/src/coin.rs#L580-L587)):

```rust
pub struct Info {
    pub nonce: Nonce,           // Random 32-byte value for uniqueness
    pub type_: ShieldedTokenType,  // Token type (32-byte hash)
    pub value: u128,            // Amount (hidden in commitments)
}
```

### 2.2 Qualified Coin Info

When a coin is added to the Merkle tree, it gains an index ([coin-structure/src/coin.rs#L680-L688](../coin-structure/src/coin.rs#L680-L688)):

```rust
pub struct QualifiedInfo {
    pub nonce: Nonce,
    pub type_: ShieldedTokenType,
    pub value: u128,
    pub mt_index: u64,  // Position in commitment Merkle tree
}
```

Contracts store `QualifiedInfo` to later construct Merkle proofs for spending.

### 2.3 Token Type

Shielded token types wrap a 32-byte hash ([coin-structure/src/coin.rs#L227-L235](../coin-structure/src/coin.rs#L227-L235)):

```rust
pub struct ShieldedTokenType(pub HashOutput);
```

Contracts derive custom token types from their address:

```rust
// coin-structure/src/contract.rs#L64-L65
pub fn custom_shielded_token_type(&self, domain_sep: HashOutput) -> ShieldedTokenType {
    ShieldedTokenType(self.custom_token_type(domain_sep))
}
```

### 2.4 Cryptographic Primitives

| Type | Definition | Purpose |
|------|------------|---------|
| `Nullifier` | `HashOutput` (32 bytes) | Marks coin as spent |
| `Commitment` | `HashOutput` (32 bytes) | Hides coin info + recipient |
| `Nonce` | `HashOutput` (32 bytes) | Ensures coin uniqueness |
| `SecretKey` | `HashOutput` (32 bytes) | Coin spending authority |
| `PublicKey` | `Hash(SecretKey)` | Coin receiving address |

---

## 3. Key Derivation

### 3.1 Wallet Seed

Keys derive from a 32-byte seed ([zswap/src/keys.rs#L37-L55](../zswap/src/keys.rs#L37-L55)):

```rust
pub struct Seed([u8; 32]);

impl Seed {
    pub fn derive_coin_secret_key(&self) -> coin::SecretKey {
        let domain_separator = b"midnight:csk";
        // Hash(domain_separator || seed)
        ...
    }

    pub fn derive_encryption_secret_key(&self) -> encryption::SecretKey {
        const DOMAIN_SEPARATOR: &[u8; 12] = b"midnight:esk";
        // Hash(domain_separator || seed) with 64-byte expansion
        ...
    }
}
```

### 3.2 Key Types

**Coin Keys** - Authorization for spending:

```rust
// coin-structure/src/coin.rs#L145-L160
pub struct SecretKey(pub HashOutput);

impl SecretKey {
    pub fn public_key(&self) -> PublicKey {
        // Hash("midnight:zswap-pk[v1]" || self)
        ...
    }
}
```

**Encryption Keys** - For sending coin info to recipients:

```rust
// transient-crypto/src/encryption.rs#L88-L95
pub struct SecretKey(EmbeddedFr);  // Scalar on embedded curve

impl SecretKey {
    pub fn public_key(&self) -> PublicKey {
        PublicKey(EmbeddedGroupAffine::generator() * self.0)
    }
}
```

### 3.3 Combined Secret Keys

Wallets hold both key types ([zswap/src/keys.rs#L102-L115](../zswap/src/keys.rs#L102-L115)):

```rust
pub struct SecretKeys {
    pub coin_secret_key: coin::SecretKey,
    pub encryption_secret_key: encryption::SecretKey,
}

impl SecretKeys {
    pub fn coin_public_key(&self) -> coin::PublicKey { ... }
    pub fn enc_public_key(&self) -> encryption::PublicKey { ... }
    pub fn try_decrypt(&self, msg: &CoinCiphertext) -> Option<CoinInfo> { ... }
}
```

---

## 4. Commitments and Nullifiers

### 4.1 Commitment Computation

Commitments bind coin info to a recipient ([coin-structure/src/coin.rs#L625-L636](../coin-structure/src/coin.rs#L625-L636)):

```rust
impl Info {
    pub fn commitment(&self, recipient: &Recipient) -> Commitment {
        let mut data = Vec::with_capacity(21 + 32 + 32 + 16 + 1 + 32);
        data.extend(b"midnight:zswap-cc[v1]");  // Domain separator
        self.binary_repr(&mut data);             // nonce || type_ || value
        match &recipient {
            Recipient::User(pk) => (true, pk.0).binary_repr(&mut data),
            Recipient::Contract(addr) => (false, addr.0).binary_repr(&mut data),
        }
        Commitment(persistent_hash(&data))
    }
}
```

**Key property**: Same coin info + different recipient = different commitment.

### 4.2 Nullifier Computation

Nullifiers use secret information to mark spending ([coin-structure/src/coin.rs#L638-L651](../coin-structure/src/coin.rs#L638-L651)):

```rust
impl Info {
    pub fn nullifier(&self, se: &SenderEvidence<'_>) -> Nullifier {
        let mut data = Vec::with_capacity(21 + 32 + 32 + 16 + 1 + 32);
        data.extend(b"midnight:zswap-cn[v1]");  // Different domain separator
        self.binary_repr(&mut data);
        match &se {
            SenderEvidence::User(sk) => (true, sk.0).binary_repr(&mut data),
            SenderEvidence::Contract(addr) => (false, addr.0).binary_repr(&mut data),
        }
        Nullifier(persistent_hash(&data))
    }
}
```

**Key property**: Only the owner (with secret key) can compute the nullifier.

### 4.3 Sender Evidence and Recipients

Two enum types for ownership ([coin-structure/src/transfer.rs#L28-L67](../coin-structure/src/transfer.rs#L28-L67)):

```rust
pub enum Recipient {
    User(PublicKey),
    Contract(ContractAddress),
}

pub enum SenderEvidence<'a> {
    User(Cow<'a, SecretKey>),
    Contract(ContractAddress),
}
```

For contracts, `ContractAddress` serves as both public identity and spending authority.

---

## 5. ZSwap State

The ledger's ZSwap state ([zswap/src/ledger.rs#L39-L47](../zswap/src/ledger.rs#L39-L47)):

```rust
pub struct State<D: DB> {
    pub coin_coms: MerkleTree<Option<Sp<ContractAddress, D>>, D>,  // Commitment tree
    pub coin_coms_set: HashMap<Commitment, (), D>,                 // Commitment set (dedup)
    pub first_free: u64,                                           // Next tree index
    pub nullifiers: HashMap<Nullifier, (), D>,                     // Spent coins
    pub past_roots: TimeFilterMap<Identity<MerkleTreeDigest>, D>,  // Historical roots
}
```

### 5.1 State Operations

**Applying an input** ([zswap/src/ledger.rs#L63-L83](../zswap/src/ledger.rs#L63-L83)):

```rust
fn apply_input<P>(&self, inp: Input<P, D>, ...) -> Result<Self, TransactionInvalid> {
    // 1. Check Merkle root is in history
    if !self.past_roots.contains(&inp.merkle_tree_root) {
        return Err(TransactionInvalid::UnknownMerkleRoot(...));
    }
    // 2. Check nullifier not already present (double-spend)
    if self.nullifiers.contains_key(&inp.nullifier) {
        return Err(TransactionInvalid::NullifierAlreadyPresent(...));
    }
    // 3. Add nullifier
    self.nullifiers = self.nullifiers.insert(inp.nullifier, ());
    Ok(self)
}
```

**Applying an output** ([zswap/src/ledger.rs#L85-L111](../zswap/src/ledger.rs#L85-L111)):

```rust
fn apply_output<P>(&self, out: Output<P, D>, ...) -> Result<(Self, Commitment, u64), ...> {
    // 1. Check commitment not already present (faerie gold attack)
    if self.coin_coms_set.contains_key(&out.coin_com) {
        return Err(TransactionInvalid::CommitmentAlreadyPresent(...));
    }
    // 2. Add to commitment set
    self.coin_coms_set = self.coin_coms_set.insert(out.coin_com, ());
    // 3. Add to Merkle tree
    self.coin_coms = self.coin_coms.update_hash(self.first_free, out.coin_com.0, ...);
    self.first_free += 1;
    Ok((self, out.coin_com, first_free))
}
```

### 5.2 Post-Block Updates

After each block, roots are archived ([zswap/src/ledger.rs#L227-L239](../zswap/src/ledger.rs#L227-L239)):

```rust
pub fn post_block_update(&self, tblock: Timestamp) -> Self {
    let mut new_st = self.clone();
    new_st.coin_coms = new_st.coin_coms.rehash();  // Recompute Merkle root
    new_st.past_roots = new_st.past_roots.insert(tblock, new_st.coin_coms.root());
    new_st.past_roots = new_st.past_roots.filter(tblock - Duration::from_secs(3600));  // 1 hour TTL
    new_st
}
```

---

## 6. ZSwap Offer Components

### 6.1 Input

Spends an existing coin ([zswap/src/structure.rs#L202-L213](../zswap/src/structure.rs#L202-L213)):

```rust
pub struct Input<P, D: DB> {
    pub nullifier: Nullifier,
    pub value_commitment: Pedersen,
    pub contract_address: Option<Sp<ContractAddress, D>>,  // If contract-owned
    pub merkle_tree_root: MerkleTreeDigest,                // For inclusion proof
    pub proof: Arc<P>,
}
```

### 6.2 Output

Creates a new coin ([zswap/src/structure.rs#L300-L310](../zswap/src/structure.rs#L300-L310)):

```rust
pub struct Output<P, D: DB> {
    pub coin_com: Commitment,
    pub value_commitment: Pedersen,
    pub contract_address: Option<Sp<ContractAddress, D>>,  // If contract-owned
    pub ciphertext: Option<Sp<CoinCiphertext, D>>,         // Encrypted coin info
    pub proof: Arc<P>,
}
```

### 6.3 Transient

Created and spent in same transaction ([zswap/src/structure.rs#L375-L386](../zswap/src/structure.rs#L375-L386)):

```rust
pub struct Transient<P, D: DB> {
    pub nullifier: Nullifier,
    pub coin_com: Commitment,
    pub value_commitment_input: Pedersen,
    pub value_commitment_output: Pedersen,
    pub contract_address: Option<Sp<ContractAddress, D>>,
    pub ciphertext: Option<Sp<CoinCiphertext, D>>,
    pub proof_input: Arc<P>,
    pub proof_output: Arc<P>,
}
```

Transients use an ephemeral single-leaf Merkle tree for inclusion proof:

```rust
// zswap/src/structure.rs#L414-L420
pub fn as_input(&self) -> Input<P, D> {
    Input {
        merkle_tree_root: MerkleTree::<_>::blank(ZSWAP_TREE_HEIGHT)
            .update_hash(0, self.coin_com.0, ())
            .rehash()
            .root(),
        ...
    }
}
```

### 6.4 Delta

Declares offer imbalance by token type ([zswap/src/structure.rs#L454-L458](../zswap/src/structure.rs#L454-L458)):

```rust
pub struct Delta {
    pub token_type: ShieldedTokenType,
    pub value: i128,  // Positive = spend more than create, Negative = create more than spend
}
```

### 6.5 Offer

Complete ZSwap transaction component ([zswap/src/structure.rs#L460-L477](../zswap/src/structure.rs#L460-L477)):

```rust
pub struct Offer<P, D: DB> {
    pub inputs: Array<Input<P, D>, D>,
    pub outputs: Array<Output<P, D>, D>,
    pub transient: Array<Transient<P, D>, D>,
    pub deltas: Array<Delta, D>,  // Must be sorted, key-unique, non-zero
}
```

---

## 7. Value Commitments and Balancing

### 7.1 Pedersen Commitments

Each input/output carries a homomorphic Pedersen commitment:

```
value_commitment = H(token_type, segment) * value + G * randomness
```

Where:
- `H(token_type, segment)` = hash-to-curve of token type and segment
- `G` = generator point
- `value` = coin value
- `randomness` = random scalar (binding factor)

### 7.2 Offer Balancing

An offer is balanced when ([spec/zswap.md#L316-L328](../spec/zswap.md#L316-L328)):

```
sum(input.value_commitment) + sum(transient.input_commitment)
- sum(output.value_commitment) - sum(transient.output_commitment)
- sum(H(delta.token_type, segment) * delta.value)
== G * rc_all
```

Where `rc_all` is the sum of all binding randomness (revealed for verification).

### 7.3 Verification

The offer's overall commitment must be openable to zero ([zswap/src/verify.rs#L284-L312](../zswap/src/verify.rs#L284-L312)):

```rust
fn offer_well_formed_common<P, D: DB>(offer: &Offer<P, D>, segment: u16) -> Result<Pedersen, ...> {
    // Compute IO commitment
    let io_com = offer.inputs.iter().map(|inp| inp.value_commitment)
        .chain(offer.outputs.iter().map(|o| -o.value_commitment))
        .chain(offer.transient.iter().map(|t| t.value_commitment_input - t.value_commitment_output))
        .fold(identity, Add::add);

    // Compute deltas commitment
    let deltas_com = offer.deltas.iter()
        .map(|d| Pedersen::commit(&(d.token_type, segment), &d.value.into(), &0.into()))
        .fold(identity, Add::add);

    Ok(io_com - deltas_com)  // Must equal G * rc_all
}
```

---

## 8. Proof System

### 8.1 Proof Keys

Three circuit types with separate proving/verifying keys ([zswap/src/structure.rs#L57-L79](../zswap/src/structure.rs#L57-L79)):

| Circuit | Purpose | Files |
|---------|---------|-------|
| `spend` | Input authorization | `spend.prover`, `spend.verifier`, `spend.bzkir` |
| `output` | Output creation | `output.prover`, `output.verifier`, `output.bzkir` |
| `sign` | Authorized claims | `sign.prover`, `sign.verifier`, `sign.bzkir` |

### 8.2 Input Proof

Proves authorization to spend ([spec/zswap.md#L113-L130](../spec/zswap.md#L113-L130)):

```rust
fn input_valid(
    input: Public<ZswapInput>,
    segment: Public<u16>,
    sk: Private<Either<SecretKey, ContractAddress>>,
    merkle_tree: Private<MerkleTree>,
    coin: Private<CoinInfo>,
    rc: Private<Scalar>,
) -> bool {
    // 1. Merkle root matches
    assert!(input.merkle_tree_root == merkle_tree.root());
    // 2. Coin is in tree
    assert!(merkle_tree.contains(coin.commitment(sk.public_key())));
    // 3. Nullifier correctly computed
    assert!(input.nullifier == coin.nullifier(sk));
    // 4. Contract ownership matches
    assert!(input.contract == match sk { User => None, Contract(c) => Some(c) });
    // 5. Value commitment correct
    let vc = H(coin.type_, segment) * coin.value + G * rc;
    assert!(input.value_commitment == vc);
}
```

### 8.3 Output Proof

Proves correct coin creation ([spec/zswap.md#L132-L145](../spec/zswap.md#L132-L145)):

```rust
fn output_valid(
    output: Public<ZswapOutput>,
    segment: Public<u16>,
    pk: Private<Either<PublicKey, ContractAddress>>,
    coin: Private<CoinInfo>,
    rc: Private<Scalar>,
) -> bool {
    // 1. Commitment correctly computed
    assert!(output.commitment == coin.commitment(pk));
    // 2. Contract ownership matches
    assert!(output.contract == match pk { User => None, Contract(c) => Some(c) });
    // 3. Value commitment correct
    let vc = H(coin.type_, segment) * coin.value + G * rc;
    assert!(output.value_commitment == vc);
}
```

### 8.4 Proof Construction

Inputs construct their proofs with ([zswap/src/construct.rs#L130-L215](../zswap/src/construct.rs#L130-L215)):

```rust
impl Input<ProofPreimage, D> {
    pub fn new_contract_owned<R: Rng>(
        rng: &mut R,
        coin: &QualifiedCoinInfo,
        segment: Option<u16>,
        contract: ContractAddress,
        tree: &MerkleTree<...>,
    ) -> Result<Self, ...> {
        let rc: EmbeddedFr = rng.gen();
        let nullifier = CoinInfo::from(coin).nullifier(&SenderEvidence::Contract(contract));
        let value_commitment = Pedersen::commit(&(coin.type_, segment), &coin.value.into(), &rc);
        let merkle_tree_root = tree.root()?;
        
        // Proof preimage contains:
        // - Secret key or contract address
        // - Merkle path
        // - Coin info
        // - Randomness
        ...
    }
}
```

---

## 9. Contract Interactions

### 9.1 Effects for Shielded Tokens

Contracts declare shielded operations via Effects ([onchain-runtime/src/context.rs#L637-L650](../onchain-runtime/src/context.rs#L637-L650)):

```rust
pub struct Effects<D: DB> {
    pub claimed_nullifiers: HashSet<Nullifier, D>,           // Spent coins
    pub claimed_shielded_receives: HashSet<Commitment, D>,   // Received coins
    pub claimed_shielded_spends: HashSet<Commitment, D>,     // Coins sent to users
    pub shielded_mints: HashMap<HashOutput, u64, D>,         // New token issuance
    // ... unshielded fields ...
}
```

### 9.2 Kernel Operations

Contracts use kernel calls to interact with ZSwap:

**Receiving a coin** (user deposits to contract):

```rust
// ledger/tests/token_vault_shielded.rs#L186
kernel_claim_zswap_coin_receive!((), (), coin_commitment)
```

**Spending a coin** (contract owns coin, nullifies it):

```rust
// ledger/tests/token_vault_shielded.rs#L471
kernel_claim_zswap_nullifier!((), (), nullifier)
```

**Sending to a user** (contract creates output for user):

```rust
// ledger/tests/token_vault_shielded.rs#L472
kernel_claim_zswap_coin_spend!((), (), user_commitment)
```

### 9.3 Contract-Owned vs User-Owned

| Aspect | User-Owned | Contract-Owned |
|--------|------------|----------------|
| `contract_address` field | `None` | `Some(ContractAddress)` |
| Spending authority | Secret key | Contract address |
| Ciphertext | Required (encrypted coin info) | Not allowed |
| Commitment recipient | `Recipient::User(pk)` | `Recipient::Contract(addr)` |

---

## 10. Transaction Flow

### 10.1 User-to-Contract Deposit

1. User creates `ZswapOutput` with `contract_address = Some(addr)`
2. User adds output to offer with negative delta
3. Contract calls `kernel.claimZswapCoinReceive(commitment)`
4. Contract stores `QualifiedCoinInfo` in state
5. Ledger adds commitment to tree, stores contract address as auxiliary data

Reference: [ledger/tests/token_vault_shielded.rs#L149-L230](../ledger/tests/token_vault_shielded.rs#L149-L230)

### 10.2 Contract-to-User Withdrawal

1. Contract creates `ZswapInput` from stored `QualifiedCoinInfo`
2. Contract creates `ZswapOutput` for user (with ciphertext)
3. Contract creates `ZswapOutput` for change (contract-owned)
4. Contract calls:
   - `kernel.claimZswapNullifier(nullifier)` - spend vault coin
   - `kernel.claimZswapCoinSpend(user_commitment)` - authorize user output
   - `kernel.claimZswapCoinSpend(change_commitment)` - authorize change
   - `kernel.claimZswapCoinReceive(change_commitment)` - receive change
5. Contract updates stored coin to change coin

Reference: [ledger/tests/token_vault_shielded.rs#L386-L548](../ledger/tests/token_vault_shielded.rs#L386-L548)

### 10.3 Contract Coin Merge

When receiving a second deposit, contracts must:

1. Nullify existing vault coin
2. Receive new deposit
3. Create merged coin with combined value
4. Store merged coin

Uses `ZswapTransient` for the new deposit (created and consumed in same tx).

Reference: [ledger/tests/token_vault_shielded.rs#L232-L384](../ledger/tests/token_vault_shielded.rs#L232-L384)

---

## 11. Coin Derivation

### 11.1 Fresh Coins

Random nonce generation ([coin-structure/src/coin.rs#L600-L608](../coin-structure/src/coin.rs#L600-L608)):

```rust
impl Info {
    pub fn new<R: Rng>(rng: &mut R, value: u128, type_: ShieldedTokenType) -> Self {
        Info {
            nonce: rng.gen(),
            value,
            type_,
        }
    }
}
```

### 11.2 Derived Coins (evolve_from)

When splitting coins, derive nonces deterministically ([coin-structure/src/coin.rs#L615-L623](../coin-structure/src/coin.rs#L615-L623)):

```rust
impl Info {
    pub fn evolve_from(&self, domain_sep: &[u8], value: u128, type_: ShieldedTokenType) -> Self {
        Info {
            nonce: Nonce(transient_hash(&[
                Fr::from_le_bytes(domain_sep),
                degrade_to_transient(self.nonce.0),
            ])),
            value,
            type_,
        }
    }
}
```

### 11.3 Domain Separator Convention

When creating multiple outputs from one input, use different domain separators:

```rust
// ledger/tests/token_vault_shielded.rs#L418-L428
let withdraw_coin = pot.evolve_from(
    b"midnight:kernel:nonce_evolve",
    WITHDRAW_AMOUNT,
    pot.type_,
);

let change_coin = pot.evolve_from(
    b"midnight:kernel:nonce_evolve/2",  // Different separator!
    pot.value - WITHDRAW_AMOUNT,
    pot.type_,
);
```

**Rationale**: Same domain separator + same parent nonce = identical derived nonce = commitment collision.

---

## 12. Encryption

### 12.1 Coin Ciphertext

Encrypts coin info for recipient ([zswap/src/structure.rs#L82-L89](../zswap/src/structure.rs#L82-L89)):

```rust
pub struct CoinCiphertext {
    pub c: EmbeddedGroupAffine,  // Diffie-Hellman challenge
    pub ciph: [Fr; 6],           // Encrypted nonce + type + value
}
```

### 12.2 Encryption Scheme

El Gamal-based with Poseidon CTR mode ([transient-crypto/src/encryption.rs#L14-L23](../transient-crypto/src/encryption.rs#L14-L23)):

1. Sender generates ephemeral `y`, computes `c = g^y`
2. Shared secret `K* = pk^y = g^{xy}`
3. Key `K = Hash(K*.x, K*.y)`
4. Encrypt: `ciph[i] = Hash(K, i) + msg[i]`

```rust
impl PublicKey {
    pub fn encrypt<R: Rng, T: FieldRepr>(&self, rng: &mut R, msg: &T) -> Ciphertext {
        let y: EmbeddedFr = rng.gen();
        let c = G * y;
        let k_star = self.0 * y;
        let k = transient_hash(&[k_star.x(), k_star.y()]);
        let ciph = msg.field_vec()
            .enumerate()
            .map(|(i, m)| transient_hash(&[k, i.into()]) + m)
            .collect();
        Ciphertext { c, ciph }
    }
}
```

### 12.3 User-Owned Output Requirement

User-owned outputs **must** include ciphertext so recipients can discover and spend coins. Contract-owned outputs **must not** include ciphertext (contracts don't have encryption keys).

```rust
// zswap/src/verify.rs#L205-L212
pub fn well_formed(&self, segment: u16) -> Result<(), MalformedOffer> {
    if let (Some(address), Some(ciphertext)) = (&self.contract_address, &self.ciphertext) {
        return Err(MalformedOffer::ContractSentCiphertext { ... });
    }
    ...
}
```

---

## Quick Reference

### Creating Shielded Transactions

| Operation | Key Functions |
|-----------|---------------|
| Create input | `Input::new_contract_owned()` |
| Create output (user) | `Output::new()` |
| Create output (contract) | `Output::new_contract_owned()` |
| Create transient | `Transient::new_from_contract_owned_output()` |
| Merge offers | `Offer::merge()` |
| Normalize offer | `Offer::normalize()` |

### Contract Kernel Calls

| Call | Purpose |
|------|---------|
| `kernel_claim_zswap_coin_receive!` | Accept incoming coin |
| `kernel_claim_zswap_nullifier!` | Spend owned coin |
| `kernel_claim_zswap_coin_spend!` | Authorize outgoing coin |

### Verification Steps

1. Merkle root in past_roots
2. Nullifier not in nullifiers set
3. Commitment not in commitment set
4. Proofs valid for all inputs/outputs
5. Offer balanced (value commitments open to declared deltas)
6. No contract ciphertexts

---

## File References

| Component | Location |
|-----------|----------|
| Coin types | [coin-structure/src/coin.rs](../coin-structure/src/coin.rs) |
| Transfer types | [coin-structure/src/transfer.rs](../coin-structure/src/transfer.rs) |
| Contract derivation | [coin-structure/src/contract.rs](../coin-structure/src/contract.rs) |
| Key management | [zswap/src/keys.rs](../zswap/src/keys.rs) |
| ZSwap structures | [zswap/src/structure.rs](../zswap/src/structure.rs) |
| ZSwap ledger state | [zswap/src/ledger.rs](../zswap/src/ledger.rs) |
| Construction | [zswap/src/construct.rs](../zswap/src/construct.rs) |
| Verification | [zswap/src/verify.rs](../zswap/src/verify.rs) |
| Encryption | [transient-crypto/src/encryption.rs](../transient-crypto/src/encryption.rs) |
| Shielded tests | [ledger/tests/token_vault_shielded.rs](../ledger/tests/token_vault_shielded.rs) |
| Specification | [spec/zswap.md](../spec/zswap.md) |
