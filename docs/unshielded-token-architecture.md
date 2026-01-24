# Unshielded Token Architecture

## Overview

Unshielded tokens in the Midnight ledger use a **hybrid model** that operates at two distinct layers. While unshielded tokens ARE UTXOs at the user-facing layer, contracts interact with them through a balance-tracking abstraction.

## The Two-Layer Model

### Layer 1: User ↔ Ledger (UTXO-Based)

At the user-facing layer, unshielded tokens are genuine UTXOs. The ledger maintains a UTXO set where each unshielded token output is represented as:

```rust
struct Utxo {
    value: u128,
    owner: PublicKey,
    type_: UnshieldedTokenType,
    intent_hash: IntentHash,
    output_no: u32,
}
```

**UTXO Operations** ([semantics.rs:1585-1630](../ledger/src/semantics.rs#L1585-L1630)):

```rust
impl<D: DB> UtxoState<D> {
    pub fn apply_offer<S: SignatureKind<D>>(
        &self,
        offer: &UnshieldedOffer<S, D>,
        ...
    ) -> Result<Self, TransactionInvalid<D>> {
        // CONSUME input UTXOs from the UTXO set
        for input in offer.inputs.iter_deref() {
            res = res.remove(&input_utxo);  // UTXO consumed
        }
        
        // CREATE output UTXOs in the UTXO set  
        for output in outputs.iter() {
            res = res.insert((**output).clone(), meta);  // UTXO created
        }
    }
}
```

Each UTXO is uniquely identified by `(intent_hash, output_no)`, ensuring no double-spending.

### Layer 2: Contract ↔ Ledger (Balance-Tracked)

At the contract layer, the ledger maintains a simple balance map per contract ([structure.rs:3028](../ledger/src/structure.rs#L3028)):

```rust
struct ContractState<D: DB> {
    data: ChargedState<D>,
    balance: storage::storage::HashMap<TokenType, u128, D>,
    // ...
}
```

**Balance Updates** ([semantics.rs:1251-1280](../ledger/src/semantics.rs#L1251-L1280)):

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

## Transaction Flow

### Example: User deposits 700 tokens into contract

```
┌─────────────────────────────────────────────────────────────────┐
│                         USER LAYER                               │
│  ┌──────────┐                              ┌──────────────┐     │
│  │ UTXO A   │ ──── consumed ────>          │  UTXO B      │     │
│  │ 1000 tok │                              │  (change)    │     │
│  └──────────┘                              │  300 tok     │     │
│       │                                    └──────────────┘     │
│       │ value=700                                ↑              │
│       ▼                                          │              │
├───────────────────── UnshieldedOffer ────────────┼──────────────┤
│  inputs: [UTXO_A]           outputs: [UTXO_B]    │              │
│       │                           ↑              │              │
│       │ consumed by ledger        │ created by ledger           │
└───────┼───────────────────────────┼──────────────┴──────────────┘
        │                           │
        ▼                           │
┌───────────────────────────────────┼─────────────────────────────┐
│                      CONTRACT LAYER                              │
│                                                                  │
│  effects.unshielded_inputs = 700   (credit balance)             │
│  effects.unshielded_outputs = 0    (no debit this tx)           │
│                                                                  │
│  Contract balance: 0 + 700 = 700                                │
└──────────────────────────────────────────────────────────────────┘
```

**What happens:**

1. User creates an `UnshieldedOffer` with:
   - `inputs: [UTXO_A (1000 tokens)]` 
   - `outputs: [UTXO_B (300 tokens to self as change)]`

2. Contract generates transcript with:
   - `effects.unshielded_inputs: [(token_type, 700)]`

3. Ledger processes transaction:
   - **UTXO layer**: Removes UTXO_A from UTXO set, adds UTXO_B to UTXO set
   - **Balance layer**: Adds 700 to `contract.balance[token_type]`

### Example: Contract sends 30 tokens to Alice

```
┌─────────────────────────────────────────────────────────────────┐
│                      CONTRACT LAYER                              │
│                                                                  │
│  effects.unshielded_outputs = 30   (debit balance)              │
│  effects.claimed_unshielded_spends = [(token_type, Alice, 30)]  │
│                                                                  │
│  Contract balance: 700 - 30 = 670                               │
└───────┬──────────────────────────────────────────────────────────┘
        │
        ▼
┌───────────────────── UnshieldedOffer ────────────────────────────┐
│  inputs: []              outputs: [UTXO_C (30 to Alice)]         │
│                                      ↓                           │
│                          created by ledger (via user's tx)       │
└──────────────────────────────────────┬───────────────────────────┘
                                       ▼
┌─────────────────────────────────────────────────────────────────┐
│                         USER LAYER                               │
│                          ┌──────────────┐                        │
│                          │  UTXO C      │                        │
│                          │  30 tok      │                        │
│                          │  owner=Alice │                        │
│                          └──────────────┘                        │
└─────────────────────────────────────────────────────────────────┘
```

**What happens:**

1. Contract execution produces:
   - `effects.unshielded_outputs: [(token_type, 30)]`
   - `effects.claimed_unshielded_spends: [(token_type, Alice, 30)]`

2. User constructs transaction with:
   - `UnshieldedOffer.outputs: [UTXO_C (30 tokens to Alice)]`

3. Ledger processes transaction:
   - **Balance layer**: Subtracts 30 from `contract.balance[token_type]`
   - **UTXO layer**: Creates UTXO_C in UTXO set with owner=Alice

## Verification: Claimed vs Real Spends

The ledger verifies that all claimed unshielded spends are legitimate ([verify.rs:1599-1640](../ledger/src/verify.rs#L1599-L1640)):

```rust
claimed_unshielded_spends ⊆ real_unshielded_spends

where:
  real_unshielded_spends = 
    (contract.unshielded_inputs)     // tokens going TO contracts
    ∪ 
    (UnshieldedOffer.outputs)        // tokens provided by user
```

This ensures:
- Contracts can only spend tokens they actually received
- Contract→user transfers are backed by real balance
- Contract→contract transfers are properly tracked

## Why No "Change Coin" for Contracts?

### Shielded Tokens (Coin Commitments)

With shielded tokens, contracts store **coin commitments** — cryptographic commitments to specific coin values. These commitments can only be consumed whole:

- Contract has: `[Coin(100), Coin(50), Coin(25)]`
- To send 30: Must consume Coin(50), create Coin(20) as change
- **Result**: Contract now has `[Coin(100), Coin(25), Coin(20)]`

The contract manipulates individual coin objects, requiring explicit change handling.

### Unshielded Tokens (Ledger Balance)

With unshielded tokens, contracts have a **ledger-tracked balance** — just a number:

- Contract has: `balance[token_type] = 700`
- To send 30: Subtract 30 from balance
- **Result**: Contract now has `balance[token_type] = 670`

The ledger does arithmetic directly on the balance. No coin selection, no change coins needed.

## Summary Comparison

| Aspect | User Layer | Contract Layer |
|--------|-----------|----------------|
| **Representation** | UTXOs | Balance (HashMap) |
| **Operations** | Consume inputs, create outputs | Add/subtract from balance |
| **Handled By** | `UtxoState.apply_offer()` | `semantics.rs` balance updates |
| **Change Handling** | User creates change UTXO in outputs | Automatic via arithmetic |
| **Verification** | Input UTXOs must exist | Balance must not go negative |

## Design Rationale

This hybrid model provides several benefits:

1. **Contract Simplicity**: Contracts don't need UTXO selection algorithms — they just declare amounts
2. **User Flexibility**: Users manage their own UTXOs and can batch multiple contract interactions
3. **Atomic Composition**: Multiple contract calls can share a single `UnshieldedOffer`
4. **Privacy Separation**: Unshielded (transparent) vs Shielded (private) have distinct mechanics

The key insight: **The contract abstraction hides UTXO mechanics behind balance semantics.** Contracts declare "I received X" or "I'm sending Y" and the ledger + user wallet handle the UTXO bookkeeping.

## Related Code Paths

- UTXO consumption/creation: [ledger/src/semantics.rs:1585-1630](../ledger/src/semantics.rs#L1585-L1630)
- Contract balance updates: [ledger/src/semantics.rs:1251-1280](../ledger/src/semantics.rs#L1251-L1280)
- Claimed spend verification: [ledger/src/verify.rs:1599-1640](../ledger/src/verify.rs#L1599-L1640)
- Balance tracking in verification: [ledger/src/verify.rs:800-860](../ledger/src/verify.rs#L800-L860)
- ContractState structure: [ledger/src/structure.rs:3028](../ledger/src/structure.rs#L3028)
- UnshieldedOffer structure: [ledger/src/structure.rs:756-810](../ledger/src/structure.rs#L756-L810)
