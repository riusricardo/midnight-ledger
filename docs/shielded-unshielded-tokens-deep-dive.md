# Shielded vs Unshielded Tokens: Technical Deep Dive

## Overview

This document provides a comprehensive technical analysis of shielded and unshielded tokens in the Midnight ledger, their architectures, compatibility mechanisms, and potential conversion paths.

**Key Finding**: No mechanism currently exists to convert between shielded and unshielded tokens, though the architecture theoretically supports it through carefully designed smart contracts.

## 1. Token Type Architecture

### 1.1 Core Type Definitions

Both shielded and unshielded tokens wrap the same underlying hash structure ([coin-structure/src/coin.rs#L227-L285](../coin-structure/src/coin.rs#L227-L285)):

```rust
pub struct ShieldedTokenType(pub HashOutput);   // 32 bytes
pub struct UnshieldedTokenType(pub HashOutput); // 32 bytes

pub enum TokenType {
    Unshielded(UnshieldedTokenType),  // Tag: 0
    Shielded(ShieldedTokenType),      // Tag: 1  
    Dust,                              // Tag: 2
}
```

The types are distinguished by a **discriminant tag** during serialization:
- Unshielded: Tag byte `0x00`
- Shielded: Tag byte `0x01`
- Dust: Tag byte `0x02`

### 1.2 Token Type Derivation

Contracts derive custom token types from their address ([coin-structure/src/contract.rs#L56-L66](../coin-structure/src/contract.rs#L56-L66)):

```rust
impl ContractAddress {
    fn custom_token_type(&self, domain_sep: HashOutput) -> HashOutput {
        let inner_domain_sep = HashOutput(*b"midnight:derive_token\0\0\0\0\0\0\0\0\0\0\0");
        persistent_commit(&(domain_sep, self.0), inner_domain_sep)
    }

    pub fn custom_shielded_token_type(&self, domain_sep: HashOutput) -> ShieldedTokenType {
        ShieldedTokenType(self.custom_token_type(domain_sep))
    }

    pub fn custom_unshielded_token_type(&self, domain_sep: HashOutput) -> UnshieldedTokenType {
        UnshieldedTokenType(self.custom_token_type(domain_sep))
    }
}
```

**Critical Observation**: A contract using the **same domain separator** can create both a `ShieldedTokenType` and an `UnshieldedTokenType` with **identical underlying hashes**. The only difference is the enum wrapper.

## 2. Shielded Token System (ZSwap)

### 2.1 Purpose and Design

Shielded tokens provide privacy-preserving transfers using zero-knowledge proofs. The system is based on a commitment/nullifier architecture similar to Zerocash.

### 2.2 Key Structures

**Coin Information** ([coin-structure/src/coin.rs#L559-L570](../coin-structure/src/coin.rs#L559-L570)):

```rust
pub struct Info {
    pub nonce: Nonce,
    pub type_: ShieldedTokenType,
    pub value: u128,
}
```

**ZSwap Offer Components** ([ledger/src/structure.rs](../ledger/src/structure.rs)):

| Structure | Description | Purpose |
|-----------|-------------|---------|
| `ZswapInput` | Spending a shielded coin | Reveals nullifier, proves ownership via ZK |
| `ZswapOutput` | Creating a shielded coin | Creates commitment, encrypts coin info |
| `ZswapTransient` | Ephemeral coin | Spend and create in same transaction |
| `ZswapOffer` | Transaction bundle | Inputs + Outputs + Transients + Deltas |

### 2.3 State Representation

The ZSwap state ([spec/zswap.md](../spec/zswap.md)) maintains:

```rust
struct ZswapState {
    commitment_tree: MerkleTree<CoinCommitment>,
    commitment_set: Set<CoinCommitment>,
    nullifiers: Set<CoinNullifier>,
    commitment_tree_history: TimeFilterMap<MerkleTreeRoot>,
}
```

**Key Properties**:
- Coins are never explicitly stored; only commitments and nullifiers exist
- The set of unspent coins is `commitment_set - nullifiers` (not directly computable)
- Merkle tree enables zero-knowledge proofs of coin existence

### 2.4 Commitment and Nullifier Computation

**Commitment** ([coin-structure/src/coin.rs#L610-L619](../coin-structure/src/coin.rs#L610-L619)):

```rust
impl Info {
    pub fn commitment(&self, recipient: &Recipient) -> Commitment {
        let mut data = Vec::with_capacity(21 + 32 + 32 + 16 + 1 + 32);
        data.extend(b"midnight:zswap-cc[v1]");
        self.binary_repr(&mut data);
        match &recipient {
            Recipient::User(d) => (true, d.0).binary_repr(&mut data),
            Recipient::Contract(d) => (false, d.0).binary_repr(&mut data),
        }
        Commitment(persistent_hash(&data))
    }
}
```

**Nullifier** ([coin-structure/src/coin.rs#L621-L632](../coin-structure/src/coin.rs#L621-L632)):

```rust
impl Info {
    pub fn nullifier(&self, se: &SenderEvidence<'_>) -> Nullifier {
        let mut data = Vec::with_capacity(21 + 32 + 32 + 16 + 1 + 32);
        data.extend(b"midnight:zswap-cn[v1]");
        self.binary_repr(&mut data);
        match &se {
            SenderEvidence::User(d) => (true, d.0).binary_repr(&mut data),
            SenderEvidence::Contract(d) => (false, d.0).binary_repr(&mut data),
        }
        let res = Nullifier(persistent_hash(&data));
        data.zeroize();
        res
    }
}
```

### 2.5 Contract Interaction with Shielded Tokens

Contracts interact via kernel operations ([ledger/tests/token_vault_shielded.rs#L30-L43](../ledger/tests/token_vault_shielded.rs#L30-L43)):

```rust
// Receiving a shielded coin
kernel_claim_zswap_coin_receive!((), (), coin_commitment)

// Spending a shielded coin
kernel_claim_zswap_nullifier!((), (), nullifier)

// Creating an output for a recipient
kernel_claim_zswap_coin_spend!((), (), coin_commitment)
```

## 3. Unshielded Token System (UTXO)

### 3.1 Purpose and Design

Unshielded tokens use a standard UTXO model with public visibility. All amounts and owners are transparent on-chain.

### 3.2 UTXO Structure

**Stored UTXO** ([ledger/src/structure.rs#L2835-L2843](../ledger/src/structure.rs#L2835-L2843)):

```rust
pub struct Utxo {
    pub value: u128,
    pub owner: UserAddress,
    pub type_: UnshieldedTokenType,
    pub intent_hash: IntentHash,
    pub output_no: u32,
}
```

**UTXO State** ([ledger/src/structure.rs#L2887-L2891](../ledger/src/structure.rs#L2887-L2891)):

```rust
pub struct UtxoState<D: DB> {
    pub utxos: HashMap<Utxo, UtxoMeta, D, NightAnn>,
}
```

### 3.3 Unshielded Offer Structure

**UnshieldedOffer** ([ledger/src/structure.rs#L756-L763](../ledger/src/structure.rs#L756-L763)):

```rust
pub struct UnshieldedOffer<S: SignatureKind<D>, D: DB> {
    pub inputs: storage::storage::Array<UtxoSpend, D>,
    pub outputs: storage::storage::Array<UtxoOutput, D>,
    pub signatures: storage::storage::Array<S::Signature<SegIntent<D>>, D>,
}
```

Unlike ZSwap offers, unshielded offers require **signatures** for authorization, not zero-knowledge proofs.

### 3.4 The Hybrid Model: User vs Contract Layer

As documented in [docs/unshielded-token-architecture.md](../docs/unshielded-token-architecture.md), unshielded tokens operate differently at two layers:

**User Layer**: True UTXOs with explicit inputs/outputs

**Contract Layer**: Balance-tracked abstraction

Contracts maintain balances ([ledger/src/structure.rs#L3028](../ledger/src/structure.rs#L3028)):

```rust
struct ContractState<D: DB> {
    data: ChargedState<D>,
    balance: storage::storage::HashMap<TokenType, u128, D>,
    // ...
}
```

Balance updates during contract execution ([ledger/src/semantics.rs#L1251-L1280](../ledger/src/semantics.rs#L1251-L1280)):

```rust
// unshielded_inputs (user→contract) = ADD to contract balance
for (token_type, val) in transcript.effects.unshielded_inputs.clone() {
    new_balance = new_balance.insert(token_type, bal.checked_add(val)?);
}

// unshielded_outputs (contract→user) = SUBTRACT from contract balance
for (token_type, val) in transcript.effects.unshielded_outputs.clone() {
    new_balance = new_balance.insert(token_type, bal.checked_sub(val)?);
}
```

### 3.5 Contract Interaction with Unshielded Tokens

Contracts use effects to declare token movements ([ledger/tests/token_vault_common.rs#L220-L280](../ledger/tests/token_vault_common.rs#L220-L280)):

```rust
// Receiving unshielded tokens (effects index 6: unshielded_inputs)
fn receive_unshielded_ops<D: DB>(token_type: TokenType, amount: u128) -> Vec<Op<...>>

// Sending unshielded tokens (effects index 7: unshielded_outputs)
fn send_unshielded_ops<D: DB>(token_type: TokenType, amount: u128) -> Vec<Op<...>>

// Claiming specific recipient (effects index 8: claimed_unshielded_spends)
fn claim_unshielded_spend_ops<D: DB>(
    token_type: TokenType,
    recipient: Recipient,
    amount: u128
) -> Vec<Op<...>>
```

## 4. Contract Effects: The Universal Interface

### 4.1 Effects Structure

Contracts declare all their token interactions via the `Effects` structure ([onchain-runtime/src/context.rs#L637-L650](../onchain-runtime/src/context.rs#L637-L650)):

```rust
pub struct Effects<D: DB> {
    // Shielded token operations
    pub claimed_nullifiers: storage::storage::HashSet<Nullifier, D>,
    pub claimed_shielded_receives: storage::storage::HashSet<CoinCommitment, D>,
    pub claimed_shielded_spends: storage::storage::HashSet<CoinCommitment, D>,
    pub shielded_mints: storage::storage::HashMap<HashOutput, u64, D>,
    
    // Unshielded token operations
    pub unshielded_inputs: storage::storage::HashMap<TokenType, u128, D>,
    pub unshielded_outputs: storage::storage::HashMap<TokenType, u128, D>,
    pub claimed_unshielded_spends: storage::storage::HashMap<
        ClaimedUnshieldedSpendsKey, 
        u128, 
        D
    >,
    pub unshielded_mints: storage::storage::HashMap<HashOutput, u64, D>,
    
    // Common operations
    pub claimed_contract_calls: storage::storage::HashSet<ClaimedContractCallsValue, D>,
}
```

### 4.2 Effects Index Constants

From test utilities ([ledger/tests/token_vault_common.rs#L580-L598](../ledger/tests/token_vault_common.rs#L580-L598)):

```rust
// Effects structure indices for VM operations
pub const EFFECTS_IDX_CLAIMED_NULLIFIERS: u8 = 0;
pub const EFFECTS_IDX_CLAIMED_SHIELDED_RECEIVES: u8 = 1;
pub const EFFECTS_IDX_CLAIMED_SHIELDED_SPENDS: u8 = 2;
pub const EFFECTS_IDX_CLAIMED_CONTRACT_CALLS: u8 = 3;
pub const EFFECTS_IDX_SHIELDED_MINTS: u8 = 4;
pub const EFFECTS_IDX_UNSHIELDED_MINTS: u8 = 5;
pub const EFFECTS_IDX_UNSHIELDED_INPUTS: u8 = 6;
pub const EFFECTS_IDX_UNSHIELDED_OUTPUTS: u8 = 7;
pub const EFFECTS_IDX_CLAIMED_UNSHIELDED_SPENDS: u8 = 8;
```

### 4.3 Minting Tokens

Contracts can mint **new** tokens of either type ([ledger/src/verify.rs#L787-L808](../ledger/src/verify.rs#L787-L808)):

```rust
// Shielded mints
for (pre_token, val) in transcript.effects.shielded_mints.clone() {
    let tt = call.address.custom_shielded_token_type(pre_token);
    update_balance(
        &mut res,
        TokenType::Shielded(tt),
        segment,
        val,
        BalanceOperation::Addition,
    )?;
}

// Unshielded mints
for (pre_token, val) in transcript.effects.unshielded_mints.clone() {
    let tt = call.address.custom_unshielded_token_type(pre_token);
    update_balance(
        &mut res,
        TokenType::Unshielded(tt),
        segment,
        val,
        BalanceOperation::Addition,
    )?;
}
```

**Key Insight**: The minting mechanism derives token types from the contract address and a domain separator (`pre_token`). A contract can mint both shielded and unshielded tokens with the same underlying hash by using the same domain separator.

## 5. Compatibility Analysis

### 5.1 Fundamental Similarities

| Aspect | Shielded | Unshielded | Match |
|--------|----------|------------|-------|
| Underlying hash type | `HashOutput` (32 bytes) | `HashOutput` (32 bytes) | YES |
| Derivation function | `custom_shielded_token_type(domain_sep)` | `custom_unshielded_token_type(domain_sep)` | Same input |
| Contract balance key | `TokenType::Shielded(hash)` | `TokenType::Unshielded(hash)` | Different variants |
| Minting authority | Contract address | Contract address | YES |

### 5.2 Critical Differences

| Aspect | Shielded | Unshielded |
|--------|----------|------------|
| **Value visibility** | Hidden in commitment | Public |
| **Authorization** | Zero-knowledge proof | Signature |
| **State storage** | Commitment tree + nullifier set | UTXO set |
| **Spending mechanism** | Prove knowledge of secret key | Sign with private key |
| **Change handling** | Must create explicit change coin | Automatic via balance |
| **Transaction proofs** | Required for all operations | Only for contract calls |

### 5.3 The Interchangeability Statement

From the specification ([spec/zswap.md](../spec/zswap.md)):

> "Shielded and unshielded tokens are **not usually interchangeable**."

The word **"usually"** is significant. It acknowledges that while the systems are architecturally separate, some conversion mechanism is theoretically possible.

## 6. Why Direct Conversion Is Not Supported

### 6.1 Different Security Models

**Shielded tokens** rely on:
- Cryptographic commitments hiding coin information
- Nullifiers preventing double-spending without revealing which coin
- Zero-knowledge proofs of ownership

**Unshielded tokens** rely on:
- Public UTXO set
- Digital signatures for authorization
- Explicit input/output tracking

Converting between these requires bridging fundamentally different security paradigms.

### 6.2 The Balance Equation Problem

Transactions must maintain balance across all token types ([ledger/src/verify.rs#L750-L850](../ledger/src/verify.rs#L750-L850)):

```rust
// Shielded: uses signed deltas
for delta in offer.deltas.iter() {
    let (val, op) = if delta.value < 0 {
        (i128::unsigned_abs(delta.value), BalanceOperation::Subtraction)
    } else {
        (delta.value as u128, BalanceOperation::Addition)
    };
    update_balance(
        &mut res,
        TokenType::Shielded(delta.token_type),
        segment,
        val,
        op,
    )?;
}

// Unshielded: uses explicit inputs/outputs
for inp in inputs.iter() {
    update_balance(
        &mut res,
        TokenType::Unshielded(inp.type_),
        segment,
        inp.value,
        BalanceOperation::Addition,
    )?;
}
```

A conversion would need to:
1. Debit one token type
2. Credit another token type
3. Maintain cryptographic binding across both systems
4. Prove value conservation

### 6.3 The TokenType Variant Barrier

Even with identical underlying hashes:

```rust
let domain_sep = HashOutput([0u8; 32]);
let shielded = contract.custom_shielded_token_type(domain_sep);
let unshielded = contract.custom_unshielded_token_type(domain_sep);

// Same inner hash
assert_eq!(shielded.0, unshielded.0);

// Different TokenType variants
assert_ne!(
    TokenType::Shielded(shielded),
    TokenType::Unshielded(unshielded)
);
```

The contract's balance map treats these as **completely separate** token types.

## 7. Potential Implementation Approaches

### 7.1 Approach A: Bridge Contract

A dedicated bridge contract could implement conversion:

**Unshielding Flow (Shielded → Unshielded)**:

1. User creates ZSwap input spending shielded coin
2. Contract receives via `kernel.claimZswapCoinReceive(commitment)`
3. Contract nullifies via `kernel.claimZswapNullifier(nullifier)`
4. Contract mints equivalent unshielded tokens via `effects.unshielded_mints`
5. Contract sends to user via `effects.claimed_unshielded_spends`
6. Ledger creates UTXO for user

**Shielding Flow (Unshielded → Shielded)**:

1. User creates UnshieldedOffer spending UTXO
2. Contract receives via `receiveUnshielded(color, amount)`
3. Contract increments balance for `TokenType::Unshielded`
4. Contract mints shielded tokens via `effects.shielded_mints`
5. User creates ZSwap output claiming the minted tokens

**Key Requirements**:
- Circuit must prove the `domain_sep` matches for both token types
- Value conservation must be cryptographically guaranteed
- Contract must maintain 1:1 peg between locked and minted tokens

**Challenges**:
- Designing the zero-knowledge circuit for value conservation
- Ensuring the contract can't mint without backing
- Handling edge cases (partial conversions, failures, etc.)

### 7.2 Approach B: Ledger-Level Protocol Support

Add native conversion operations to the ledger:

**New Effects Fields**:

```rust
pub struct Effects<D: DB> {
    // ... existing fields ...
    
    // New conversion operations
    pub shield_conversions: HashMap<UnshieldedTokenType, u128, D>,
    pub unshield_conversions: HashMap<ShieldedTokenType, u128, D>,
}
```

**Verification Logic** ([ledger/src/verify.rs](../ledger/src/verify.rs)):

```rust
// Verify shield conversion
for (unshielded_type, amount) in effects.shield_conversions {
    // Ensure corresponding shielded mint exists
    let shielded_type = derive_corresponding_shielded_type(unshielded_type);
    assert_eq!(effects.shielded_mints.get(shielded_type), Some(amount));
    
    // Debit unshielded balance
    update_balance(TokenType::Unshielded(unshielded_type), amount, Subtraction);
}
```

**Pros**:
- Native support, most efficient
- Clear semantics
- No need for intermediate contracts

**Cons**:
- Requires protocol-level changes
- Must be carefully designed to prevent exploits
- Backward compatibility concerns

### 7.3 Approach C: Wrapped Token Pattern

Create wrapped versions that explicitly bridge:

```
User Shielded NIGHT ←→ Bridge Contract ←→ Wrapped Unshielded NIGHT
```

**Implementation**:
- `WrappedShieldedNIGHT`: Unshielded token backed by locked shielded NIGHT
- `WrappedUnshieldedNIGHT`: Shielded token backed by locked unshielded NIGHT
- Contract maintains 1:1 reserve

**Pros**:
- Familiar pattern from DeFi ecosystems
- Can be implemented entirely in contracts
- Auditable reserves

**Cons**:
- Introduces additional token types
- Users must manage wrapped vs native tokens
- Liquidity fragmentation

## 8. Implementation Recommendations

### 8.1 If Implementing Bridge Contract

**Required Components**:

1. **State Storage**:
   ```rust
   struct BridgeState {
       shielded_reserves: Map<ShieldedTokenType, Vec<QualifiedCoinInfo>>,
       unshielded_reserves: Map<UnshieldedTokenType, u128>,
       pending_conversions: Map<ConversionId, ConversionRequest>,
   }
   ```

2. **Conversion Circuits**:
   - `shield.compact`: Prove UTXO → ZSwap conversion
   - `unshield.compact`: Prove ZSwap → UTXO conversion

3. **Verification**:
   - Ensure token type hashes match
   - Prove value conservation
   - Check sufficient reserves

**Test Cases** (following pattern from [ledger/tests/token_vault_shielded.rs](../ledger/tests/token_vault_shielded.rs)):

```rust
#[tokio::test]
async fn test_shield_conversion() {
    // 1. Deploy bridge contract
    // 2. Create unshielded UTXO
    // 3. Call bridge.shield(amount)
    // 4. Verify shielded output created
    // 5. Verify UTXO consumed
    // 6. Verify contract reserves updated
}

#[tokio::test]
async fn test_unshield_conversion() {
    // 1. Deploy bridge contract
    // 2. Create shielded coin
    // 3. Call bridge.unshield(amount)
    // 4. Verify UTXO created
    // 5. Verify coin nullified
    // 6. Verify contract reserves updated
}
```

### 8.2 Circuit Design Considerations

The conversion circuit must prove ([spec/contracts.md](../spec/contracts.md)):

```
circuit shield_conversion {
    // Public inputs
    public unshielded_type: UnshieldedTokenType,
    public amount: u128,
    public output_commitment: Commitment,
    
    // Private inputs
    private domain_sep: HashOutput,
    private shielded_type: ShieldedTokenType,
    private coin_info: CoinInfo,
    
    // Constraints
    assert(unshielded_type.0 == domain_sep);
    assert(shielded_type.0 == domain_sep);
    assert(coin_info.type_ == shielded_type);
    assert(coin_info.value == amount);
    assert(output_commitment == coin_info.commitment(contract_address));
}
```

### 8.3 Security Considerations

**Reserve Validation**:
- Contract must not mint more than it holds in reserves
- Atomic operations prevent partial states
- Events for auditability

**Token Type Validation**:
- Verify domain separators match
- Prevent creating unrelated token types
- Reject conversions between incompatible types

**Reentrancy Protection**:
- Lock reserves during conversion
- Use checks-effects-interactions pattern
- Verify state consistency after operations

## 9. Code Reference Quick Links

### Core Type Definitions
- Token types: [coin-structure/src/coin.rs#L227-L285](../coin-structure/src/coin.rs#L227-L285)
- Contract derivation: [coin-structure/src/contract.rs#L56-L66](../coin-structure/src/contract.rs#L56-L66)

### Shielded Tokens
- Coin info: [coin-structure/src/coin.rs#L559-L570](../coin-structure/src/coin.rs#L559-L570)
- Commitment: [coin-structure/src/coin.rs#L610-L619](../coin-structure/src/coin.rs#L610-L619)
- Nullifier: [coin-structure/src/coin.rs#L621-L632](../coin-structure/src/coin.rs#L621-L632)

### Unshielded Tokens
- UTXO structure: [ledger/src/structure.rs#L2835-L2843](../ledger/src/structure.rs#L2835-L2843)
- UTXO state: [ledger/src/structure.rs#L2887-L2891](../ledger/src/structure.rs#L2887-L2891)
- UnshieldedOffer: [ledger/src/structure.rs#L756-L763](../ledger/src/structure.rs#L756-L763)

### Contract Interaction
- Effects structure: [onchain-runtime/src/context.rs#L637-L650](../onchain-runtime/src/context.rs#L637-L650)
- Balance updates: [ledger/src/semantics.rs#L1251-L1280](../ledger/src/semantics.rs#L1251-L1280)
- Minting: [ledger/src/verify.rs#L787-L808](../ledger/src/verify.rs#L787-L808)

### Test Examples
- Shielded token tests: [ledger/tests/token_vault_shielded.rs](../ledger/tests/token_vault_shielded.rs)
- Unshielded token tests: [ledger/tests/token_vault_unshielded.rs](../ledger/tests/token_vault_unshielded.rs)
- Test utilities: [ledger/tests/token_vault_common.rs](../ledger/tests/token_vault_common.rs)

### Specifications
- ZSwap specification: [spec/zswap.md](../spec/zswap.md)
- Contracts specification: [spec/contracts.md](../spec/contracts.md)
- Unshielded architecture: [docs/unshielded-token-architecture.md](../docs/unshielded-token-architecture.md)

## 10. Conclusion

**Current State**: Shielded and unshielded tokens operate as completely separate systems with no conversion mechanism.

**Compatibility**: Both types wrap the same `HashOutput` structure and can be derived with matching hashes using the same domain separator. However, the `TokenType` enum discriminant creates a fundamental separation in the ledger's accounting.

**Feasibility**: Conversion is theoretically possible through:
1. **Bridge Contract** (most practical for current architecture)
2. **Protocol-level support** (cleanest but requires ledger changes)
3. **Wrapped token pattern** (most complex, standard DeFi approach)

**Next Steps**: If implementing conversion, the bridge contract approach is recommended as it:
- Works within the existing architecture
- Provides clear separation of concerns
- Can be audited and tested independently
- Follows established patterns from DeFi ecosystems

The key technical challenge is designing zero-knowledge circuits that prove value conservation while bridging the different security models of shielded and unshielded tokens.
