# Technical Analysis: Shielded vs Unshielded Tokens

## Overview

This document provides a comprehensive technical analysis of shielded and unshielded tokens in the Midnight ledger, their architecture, compatibility, and potential conversion mechanisms.

**Key Finding**: No mechanism currently exists to convert between shielded and unshielded tokens, though the architecture theoretically supports it through a carefully designed smart contract.

---

## 1. Token Type Architecture

### 1.1 Type Definitions

Location: `coin-structure/src/coin.rs` (lines 227-285)

Both token types are wrappers around a 32-byte hash:

```rust
pub struct ShieldedTokenType(pub HashOutput);   // 32 bytes
pub struct UnshieldedTokenType(pub HashOutput); // 32 bytes

pub enum TokenType {
    Unshielded(UnshieldedTokenType),  // Serialization tag: 0
    Shielded(ShieldedTokenType),      // Serialization tag: 1  
    Dust,                              // Serialization tag: 2
}
```

**Critical Insight**: Both types share the same underlying `HashOutput` structure but are distinguished by a discriminant tag during serialization.

### 1.2 Token Type Derivation

Location: `coin-structure/src/contract.rs` (lines 56-66)

Contracts derive custom token types from their address:

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

**Important**: Using the same domain separator, a contract can create both `ShieldedTokenType` and `UnshieldedTokenType` with identical underlying hashes. This is the foundation for potential interoperability.

---

## 2. The Two Token Systems

### 2.1 Shielded Tokens (ZSwap)

**Purpose**: Privacy-preserving tokens using zero-knowledge proofs

**Key Components** (defined in `ledger/src/structure.rs`):

| Structure | Description |
|-----------|-------------|
| `ZswapInput` | Spending a shielded coin (reveals nullifier, hides value) |
| `ZswapOutput` | Creating a shielded coin (creates commitment) |
| `ZswapTransient` | Spend and create in same transaction |
| `ZswapOffer` | Bundle of inputs, outputs, transients, and deltas |

**State Representation** (from `spec/zswap.md`):

```rust
struct ZswapState {
    commitment_tree: MerkleTree<CoinCommitment>,
    commitment_set: Set<CoinCommitment>,
    nullifiers: Set<CoinNullifier>,
    commitment_tree_history: TimeFilterMap<MerkleTreeRoot>,
}
```

**Coin Information**:

```rust
pub struct Info {
    pub nonce: Nonce,
    pub type_: ShieldedTokenType,
    pub value: u128,
}
```

**Cryptographic Primitives**:
- Commitment: `Hash<(CoinInfo, ZswapCoinPublicKey)>`
- Nullifier: `Hash<(CoinInfo, ZswapCoinSecretKey)>`
- Value commitments: Pedersen commitments on embedded curve

### 2.2 Unshielded Tokens (UTXO)

**Purpose**: Transparent tokens with public visibility

**Key Components** (defined in `ledger/src/structure.rs`, lines 2835-2893):

| Structure | Description |
|-----------|-------------|
| `Utxo` | A stored unshielded output |
| `UtxoSpend` | Input reference to spend a UTXO |
| `UtxoOutput` | New UTXO to create |
| `UnshieldedOffer` | Bundle of inputs and outputs (no ZK proofs) |

**State Representation**:

```rust
struct Utxo {
    pub value: u128,
    pub owner: UserAddress,
    pub type_: UnshieldedTokenType,
    pub intent_hash: IntentHash,
    pub output_no: u32,
}

struct UtxoState<D: DB> {
    pub utxos: HashMap<Utxo, UtxoMeta, D, NightAnn>,
}
```

**Authorization**: Signature-based using standard Ed25519 signatures

---

## 3. Contract Interaction with Both Token Types

### 3.1 Hybrid Balance Model

Location: `docs/unshielded-token-architecture.md`

Contracts maintain a unified balance map that can hold both token types:

```rust
struct ContractState<D: DB> {
    data: ChargedState<D>,
    balance: storage::storage::HashMap<TokenType, u128, D>,
    // ...
}
```

**Key Point**: The `TokenType` enum allows contracts to track both shielded and unshielded balances in a single map, but as separate entries.

### 3.2 The Effects Structure

Location: `onchain-runtime/src/context.rs` (lines 637-650)

Contracts declare token interactions via the `Effects` structure:

```rust
pub struct Effects<D: DB> {
    // Shielded token operations
    pub claimed_nullifiers: HashSet<Nullifier, D>,
    pub claimed_shielded_receives: HashSet<CoinCommitment, D>,
    pub claimed_shielded_spends: HashSet<CoinCommitment, D>,
    pub shielded_mints: HashMap<HashOutput, u64, D>,
    
    // Unshielded token operations
    pub unshielded_inputs: HashMap<TokenType, u128, D>,
    pub unshielded_outputs: HashMap<TokenType, u128, D>,
    pub claimed_unshielded_spends: HashMap<ClaimedUnshieldedSpendsKey, u128, D>,
    pub unshielded_mints: HashMap<HashOutput, u64, D>,
    
    // Contract calls
    pub claimed_contract_calls: HashSet<ClaimedContractCallsValue, D>,
}
```

### 3.3 Token Flow in Contracts

**Shielded Token Flow**:
1. User creates `ZswapOutput` with contract ownership
2. Contract claims via `kernel.claimZswapCoinReceive(commitment)`
3. Contract stores coin in state (as `QualifiedCoinInfo`)
4. Contract can spend via `kernel.claimZswapNullifier(nullifier)`
5. Contract creates new output for recipient

**Unshielded Token Flow**:
1. User creates `UnshieldedOffer` with UTXO inputs
2. Contract declares receipt via `effects.unshielded_inputs`
3. Ledger adds to contract's balance map
4. Contract declares sends via `effects.unshielded_outputs`
5. Ledger subtracts from balance and verifies against `UnshieldedOffer.outputs`

### 3.4 Balance Verification

Location: `ledger/src/verify.rs` (lines 750-850)

During transaction verification, the ledger checks balance conservation:

```rust
// For shielded mints
for (pre_token, val) in transcript.effects.shielded_mints.clone() {
    let tt = call.address.custom_shielded_token_type(pre_token);
    update_balance(&mut res, TokenType::Shielded(tt), segment, val, Addition)?;
}

// For unshielded mints  
for (pre_token, val) in transcript.effects.unshielded_mints.clone() {
    let tt = call.address.custom_unshielded_token_type(pre_token);
    update_balance(&mut res, TokenType::Unshielded(tt), segment, val, Addition)?;
}

// Unshielded inputs (tokens to contract)
for (tt, val) in transcript.effects.unshielded_inputs.clone() {
    update_balance(&mut res, tt, segment, val, Subtraction)?;
}

// Unshielded outputs (tokens from contract)
for (tt, val) in transcript.effects.unshielded_outputs.clone() {
    update_balance(&mut res, tt, segment, val, Addition)?;
}
```

---

## 4. Compatibility Analysis

### 4.1 Comparison Matrix

| Aspect | Shielded | Unshielded | Compatible? |
|--------|----------|------------|-------------|
| **Underlying hash** | `HashOutput` (32 bytes) | `HashOutput` (32 bytes) | Yes |
| **Serialization tag** | 1 | 0 | No |
| **Storage mechanism** | Merkle tree + commitment set | UTXO set | No |
| **Authorization** | ZK proof + nullifier | Signature | No |
| **Value visibility** | Hidden in commitment | Public | No |
| **Balance tracking** | Coin commitments | Direct u128 values | No |
| **Contract balance key** | `TokenType::Shielded(hash)` | `TokenType::Unshielded(hash)` | No (separate keys) |
| **Minting** | `shielded_mints` effect | `unshielded_mints` effect | No |
| **Transaction inclusion** | `ZswapOffer` | `UnshieldedOffer` | No |

### 4.2 The Fundamental Barrier

From `spec/zswap.md`:

> "Shielded and unshielded tokens are not usually interchangeable."

The word "usually" suggests the architecture doesn't fundamentally prevent conversion, but no mechanism currently exists.

### 4.3 Why They're Different

**Value Representation**:
- Shielded: Value hidden inside `Hash<(CoinInfo, PublicKey)>`
- Unshielded: Value directly visible as `u128`

**Proof Systems**:
- Shielded: Requires ZK-SNARK proofs for every spend/create
- Unshielded: Uses simple Ed25519 signatures

**State Management**:
- Shielded: Commitment tree, nullifier set, root history
- Unshielded: Simple UTXO set with public metadata

---

## 5. Why Conversion Is Challenging

### 5.1 The Token Type Enum Problem

Even with the same underlying `HashOutput`, the enum discriminant creates different types:

```rust
let domain_sep = HashOutput([0u8; 32]);

// Both use the same hash...
let shielded = contract.custom_shielded_token_type(domain_sep);
let unshielded = contract.custom_unshielded_token_type(domain_sep);
assert_eq!(shielded.0, unshielded.0);  // Same hash

// ...but different TokenType variants
let tt1 = TokenType::Shielded(shielded);
let tt2 = TokenType::Unshielded(unshielded);
assert_ne!(tt1, tt2);  // Different types in the ledger
```

### 5.2 Balance Equation Constraints

Location: `ledger/src/verify.rs`

Transactions must maintain balance across all token types. The ledger computes a balance map:

```rust
type Balance = HashMap<(u16, TokenType), i128>;
```

where the value must sum to zero for the transaction to be valid. Since `TokenType::Shielded(x)` and `TokenType::Unshielded(x)` are different keys, they don't naturally cancel out.

### 5.3 Proof Complexity

Creating a valid conversion requires:

1. Proving ownership of the source token
2. Proving the destination token has the same value
3. Proving the underlying token type hash matches
4. Ensuring proper authorization for both systems

This requires circuits that understand both shielded and unshielded token mechanics.

---

## 6. Potential Implementation Approaches

### 6.1 Option A: Bridge Contract

**Concept**: A dedicated contract that accepts one token type and mints the corresponding type.

**Implementation Outline**:

```rust
// Unshielding: Shielded -> Unshielded
circuit unshield {
    // 1. Receive shielded coin
    let coin = kernel.claimZswapCoinReceive(commitment);
    
    // 2. Verify it matches expected domain separator
    assert!(coin.type_ == expected_shielded_type);
    
    // 3. Nullify the shielded coin
    kernel.claimZswapNullifier(nullifier);
    
    // 4. Mint equivalent unshielded tokens
    effects.unshielded_mints.insert(domain_sep, coin.value);
    
    // 5. Send to recipient
    let recipient = PublicAddress::User(recipient_key);
    effects.claimed_unshielded_spends.insert(
        (TokenType::Unshielded(unshielded_type), recipient),
        coin.value
    );
}

// Shielding: Unshielded -> Shielded
circuit shield {
    // 1. Receive unshielded tokens
    receiveUnshielded(token_color, amount);
    
    // 2. Mint equivalent shielded tokens
    effects.shielded_mints.insert(domain_sep, amount);
    
    // 3. Create ZSwap output for recipient
    // (requires special handling in transaction construction)
}
```

**Challenges**:
- The `shielded_mints` and `unshielded_mints` produce different `TokenType` variants
- Need to ensure 1:1 value conservation
- Requires careful circuit design for both directions

**Advantages**:
- Clean separation of concerns
- Auditable and upgradeable
- Doesn't require protocol changes

### 6.2 Option B: Ledger-Level Support

**Concept**: Modify the ledger to natively support conversion.

**Required Changes**:

1. Add new effect types:
   ```rust
   pub struct Effects<D: DB> {
       // ... existing fields
       pub shield_conversions: HashMap<HashOutput, u128, D>,
       pub unshield_conversions: HashMap<HashOutput, u128, D>,
   }
   ```

2. Modify balance verification (in `ledger/src/verify.rs`):
   ```rust
   // During balance checking
   for (hash, val) in effects.shield_conversions {
       // Remove from unshielded
       update_balance(&mut res, TokenType::Unshielded(UnshieldedTokenType(hash)), ...)?;
       // Add to shielded
       update_balance(&mut res, TokenType::Shielded(ShieldedTokenType(hash)), ...)?;
   }
   ```

3. Add validation logic to ensure:
   - Same underlying hash
   - Proper authorization
   - Value conservation

**Advantages**:
- Most efficient (native support)
- No intermediate contract needed
- Clear semantics

**Disadvantages**:
- Requires protocol changes
- More invasive
- Harder to upgrade/modify

### 6.3 Option C: Token Wrapper Pattern

**Concept**: Create explicit wrapper tokens with 1:1 pegs.

**Implementation**:
- Define wrapped token types: `wNIGHT_shielded`, `wNIGHT_unshielded`
- Maintain contract that always allows unwrapping to native NIGHT
- Users explicitly wrap/unwrap as needed

**Advantages**:
- Familiar pattern from DeFi
- Clear ownership model
- Flexible

**Disadvantages**:
- Additional complexity
- Two token types per asset
- Requires user education

---

## 7. Recommended Implementation Path

### Phase 1: Research & Design

1. **Circuit Design**: Create ZK circuits that can:
   - Prove shielded coin ownership
   - Verify domain separator matching
   - Ensure value conservation

2. **Token Type Analysis**: Determine if:
   - New minting effects are needed
   - Balance verification needs modification
   - Transaction structure requires changes

3. **Security Review**: Analyze:
   - Double-spend prevention
   - Value conservation guarantees
   - Authorization model

### Phase 2: Bridge Contract Implementation

1. Implement unshielding circuit:
   ```
   Input: Shielded coin with commitment C, nullifier N
   Output: Unshielded UTXO with matching value
   Constraint: Underlying token hash must match
   ```

2. Implement shielding circuit:
   ```
   Input: Unshielded UTXO
   Output: Shielded coin with commitment
   Constraint: Underlying token hash must match
   ```

3. Handle the minting mechanics:
   - Use `shielded_mints` / `unshielded_mints` with same domain separator
   - Ensure ledger accepts both directions
   - Add verification that domain separators match

### Phase 3: Testing

Comprehensive test coverage needed:

1. Unit tests for each direction
2. Integration tests with token-vault pattern
3. Stress tests for balance conservation
4. Security tests for edge cases

Test files to create:
- `ledger/tests/token_bridge_shield.rs`
- `ledger/tests/token_bridge_unshield.rs`
- `integration-tests/src/test/proving/TokenBridge.test.ts`

---

## 8. Code Reference Guide

### Core Type Definitions

| File | Lines | Content |
|------|-------|---------|
| `coin-structure/src/coin.rs` | 227-285 | Token type enum and wrappers |
| `coin-structure/src/coin.rs` | 559-570 | Shielded coin info |
| `coin-structure/src/contract.rs` | 56-66 | Token type derivation |

### Transaction Structures

| File | Lines | Content |
|------|-------|---------|
| `ledger/src/structure.rs` | 756-810 | `UnshieldedOffer` |
| `ledger/src/structure.rs` | 2835-2893 | UTXO structures |
| `zswap/src/lib.rs` | - | `ZswapOffer` and related |

### Semantics & Verification

| File | Lines | Content |
|------|-------|---------|
| `ledger/src/semantics.rs` | 1251-1280 | Contract balance updates |
| `ledger/src/semantics.rs` | 1585-1630 | UTXO state application |
| `ledger/src/verify.rs` | 750-850 | Balance verification |
| `ledger/src/verify.rs` | 1467-1533 | Shielded coin verification |
| `ledger/src/verify.rs` | 1599-1640 | Unshielded spend verification |

### Contract Runtime

| File | Lines | Content |
|------|-------|---------|
| `onchain-runtime/src/context.rs` | 637-650 | Effects structure |
| `onchain-runtime/src/transcript.rs` | 44-50 | Transcript structure |

### Test Examples

| File | Description |
|------|-------------|
| `ledger/tests/token_vault_shielded.rs` | Shielded token operations |
| `ledger/tests/token_vault_unshielded.rs` | Unshielded token operations |
| `ledger/tests/token_vault_common.rs` | Shared test utilities |
| `integration-tests/src/test/no-proving/TokenVaultShielded.test.ts` | TS shielded tests |
| `integration-tests/src/test/no-proving/TokenVaultUnshielded.test.ts` | TS unshielded tests |

### Specifications

| File | Content |
|------|---------|
| `spec/zswap.md` | ZSwap protocol specification |
| `spec/contracts.md` | Contract system specification |
| `docs/unshielded-token-architecture.md` | Unshielded token architecture |

---

## 9. Open Questions

1. **Minting Semantics**: Should conversion count as "minting" or should there be a dedicated conversion effect?

2. **Domain Separator**: Should the bridge enforce that shielded and unshielded types use the same domain separator, or allow arbitrary conversions?

3. **Fee Model**: How should conversion fees be charged? Same as minting, or special rates?

4. **Rate Limiting**: Should conversions be rate-limited or capped to prevent potential exploits?

5. **Multi-Directional**: Should a single transaction be able to both shield and unshield tokens?

---

## 10. Conclusion

**Current State**: The Midnight ledger maintains two completely separate token systems with no conversion mechanism.

**Technical Feasibility**: Conversion is architecturally possible due to shared underlying hash types, but requires:
- Careful circuit design
- Proper balance verification
- Authorization handling for both systems

**Recommended Approach**: Implement a bridge contract as Phase 1, with potential migration to ledger-level support in Phase 2 after proving the concept.

**Next Steps**:
1. Design the unshielding circuit
2. Design the shielding circuit
3. Implement proof of concept
4. Conduct security review
5. Deploy and test on testnet

---

## Appendix: Key Takeaways for Developers

- Shielded and unshielded tokens share the same `HashOutput` type but different `TokenType` enum variants
- Contracts can hold both types in their balance map as separate entries
- No current mechanism exists for conversion
- The architecture supports conversion through minting with matched domain separators
- Implementation requires ZK circuits that prove value conservation and authorization
- A bridge contract is the most practical first step

---

*Document Version*: 1.0  
*Last Updated*: January 20, 2026  
*Analysis Scope*: midnight-ledger codebase (current state)
