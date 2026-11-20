//! The tags konter writes on transactions itself (docs/spec.md §1). Whatever
//! reads the books goes by them, so a rule's own tags can't use these names:
//! a transaction could otherwise pass for a transfer or a subscription payment.

/// The bank transaction a ledger transaction was made from.
pub const BANK_ID: &str = "bank-id";
/// The rule that mapped it, or `pin`, `unmatched` or `invalid`.
pub const RULE: &str = "rule";
/// The purchase date, when it isn't the booking date.
pub const PURCHASED: &str = "purchased";
/// The amount in the currency it was paid in, for a purchase abroad.
pub const ORIGINAL: &str = "original";
/// On one side of a transfer between two imported sources: the other source.
pub const TRANSFER: &str = "transfer";
/// On a paired side of a transfer: the bank transaction that is the other side.
pub const TRANSFER_PAIR: &str = "transfer-pair";
/// On a subscription's due dates, and on the payments that settle them.
pub const SUBSCRIPTION: &str = "subscription";
/// On the transfers that pay a transfer subscription. It isn't
/// `subscription`, which marks payments that stand in for income or spending.
pub const PAYS: &str = "pays";
/// On an opening balance or a balance check: the source it's for.
pub const SOURCE: &str = "source";
/// On a bank transaction that the store has something unsettled about: what
/// (`maybe-same-as`, `conflicts`, or both with a `+` between).
pub const UNSETTLED: &str = "unsettled";

/// Every one of them.
pub const ALL: [&str; 10] =
    [BANK_ID, RULE, PURCHASED, ORIGINAL, TRANSFER, TRANSFER_PAIR, SUBSCRIPTION, PAYS, SOURCE, UNSETTLED];
