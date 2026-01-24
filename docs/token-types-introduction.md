# Introduction to Midnight Tokens

This document provides a foundational understanding of the two token systems in Midnight: **shielded** and **unshielded** tokens. For detailed implementation specifics, see the advanced documentation.

## Two Token Systems

Midnight supports two distinct token systems designed for different use cases:

| Aspect | Shielded Tokens | Unshielded Tokens |
|--------|-----------------|-------------------|
| Privacy | Values and owners hidden | Values and owners public |
| System name | ZSwap | UTXO |
| Authorization | Zero-knowledge proofs | Digital signatures |
| Primary use | Privacy-preserving transfers | Transparent, auditable transfers |

These are **separate systems**. A shielded token and an unshielded token are different types, even if they represent the same underlying asset.

---

## Shielded Tokens (ZSwap)

### What They Are

Shielded tokens provide privacy. When you hold shielded tokens:
- No one can see how many tokens you have
- No one can see who sent them to you
- No one can link your transactions together

### How Privacy Works

The ledger never stores coins directly. Instead, it stores:

1. **Commitments**: A cryptographic hash that hides the coin's value and owner
2. **Nullifiers**: A marker that indicates a coin has been spent

When you receive tokens, a commitment is added to the ledger. When you spend them, a nullifier is added. These two values cannot be linked together by anyone except the coin owner.

```
Commitment = Hash(nonce, token_type, value, recipient)
Nullifier  = Hash(nonce, token_type, value, secret_key)
```

### Spending Shielded Tokens

To spend a shielded coin, you must prove:
1. You know the secret key for a coin that exists (its commitment is in the ledger)
2. You haven't already spent it (its nullifier is not in the ledger)

This proof reveals nothing about which coin you're spending or its value.

### Key Structures

A shielded coin contains:
- `nonce`: Random value ensuring uniqueness
- `type_`: The token type (which asset)
- `value`: The amount

Reference: [coin-structure/src/coin.rs#L580-L587](coin-structure/src/coin.rs#L580-L587)

---

## Unshielded Tokens (UTXO)

### What They Are

Unshielded tokens are transparent. All information is public:
- The amount in each UTXO
- Who owns it
- The full transaction history

### How They Work

Unshielded tokens use a standard UTXO (Unspent Transaction Output) model:

1. **UTXOs**: Discrete units of value with a public owner
2. **Spending**: Requires a digital signature from the owner
3. **Creating**: Transaction outputs become new UTXOs

```
UTXO = {
    value: 100,
    owner: <public_address>,
    type_: <token_type>,
    ...
}
```

### Spending Unshielded Tokens

To spend an unshielded token:
1. Reference the UTXO you want to spend
2. Sign the transaction with your private key
3. The UTXO is consumed and new UTXOs are created

Reference: [ledger/src/structure.rs#L2835-L2843](ledger/src/structure.rs#L2835-L2843)

---

## Token Types

Both systems use token types to distinguish different assets.

### Shielded Token Type

```rust
pub struct ShieldedTokenType(pub HashOutput);  // 32 bytes
```

### Unshielded Token Type

```rust
pub struct UnshieldedTokenType(pub HashOutput);  // 32 bytes
```

Both wrap the same 32-byte hash, but they are treated as **different types** by the ledger.

Reference: [coin-structure/src/coin.rs#L227-L270](coin-structure/src/coin.rs#L227-L270)

### Native Token (NIGHT/Dust)

The native token exists in both forms:
- **Shielded NIGHT**: Used for private value transfer
- **Unshielded NIGHT (Dust)**: Used for transaction fees

---

## Contract Interactions

Contracts can hold and transfer both token types.

### Receiving Tokens

| Shielded | Unshielded |
|----------|------------|
| Contract claims a coin commitment | Contract receives UTXO value into balance |
| Stores `QualifiedCoinInfo` in state | Balance tracked as `Map<TokenType, u128>` |

### Sending Tokens

| Shielded | Unshielded |
|----------|------------|
| Contract nullifies its coin, creates new output | Contract debits balance, creates UTXO output |
| Requires ZK proof construction | Requires specifying recipient address |

### Kernel Calls

Contracts use kernel operations to interact with tokens:

**Shielded operations:**
- `claimZswapCoinReceive(commitment)` - Accept incoming coin
- `claimZswapNullifier(nullifier)` - Spend owned coin
- `claimZswapCoinSpend(commitment)` - Authorize outgoing coin

**Unshielded operations:**
- `receiveUnshielded(type, amount)` - Accept UTXO value
- `sendUnshielded(type, amount, recipient)` - Create UTXO for recipient

---

## Comparison Summary

| Feature | Shielded | Unshielded |
|---------|----------|------------|
| Value visibility | Hidden | Public |
| Owner visibility | Hidden | Public |
| Transaction linkability | Unlinkable | Fully traceable |
| Storage | Commitment tree + nullifier set | UTXO set |
| Spending proof | Zero-knowledge proof | Digital signature |
| Contract storage | `QualifiedCoinInfo` struct | Balance in `HashMap` |
| Change handling | Must create explicit change coin | Automatic via balance |

---

## Interoperability

Shielded and unshielded tokens are **not directly interchangeable**. They are separate systems with different:

- Type representations (`ShieldedTokenType` vs `UnshieldedTokenType`)
- Storage mechanisms (commitments vs UTXOs)
- Authorization methods (ZK proofs vs signatures)

A contract using the same domain separator can create tokens of both types with the same underlying hash, but the ledger treats them as distinct token types.

Reference: [spec/zswap.md](../spec/zswap.md) states: "Shielded and unshielded tokens are not usually interchangeable."

---

## When to Use Each

### Use Shielded Tokens When:

- Privacy is required (financial privacy, confidential transactions)
- You need unlinkable transactions
- Values should not be publicly visible

### Use Unshielded Tokens When:

- Transparency is required (auditing, compliance)
- Simpler transaction logic is preferred
- Public accountability is needed

---

## Next Steps

For detailed implementation information:

- **Shielded tokens**: [shielded-token-architecture.md](shielded-token-architecture.md)
- **Unshielded tokens**: [unshielded-token-architecture.md](unshielded-token-architecture.md)
- **Compatibility analysis**: [shielded-unshielded-tokens-deep-dive.md](shielded-unshielded-tokens-deep-dive.md)
