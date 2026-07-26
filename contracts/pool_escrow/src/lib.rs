#![no_std]

#[cfg(test)]
mod test;

use soroban_sdk::{
    contract, contractevent, contracterror, contractimpl, contracttype, Address, Env, String,
};

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EscrowStatus {
    Locked,
    Released,
    Refunded,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowRecord {
    pub invoice_id: String,
    pub funder: Address,
    pub warung: Address,
    pub supplier: Address,
    pub cooperative: Address,
    pub amount: i128,
    pub repaid_amount: i128,
    pub status: EscrowStatus,
    pub locked_at: u64,
    pub closed_at: u64,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Invoice(String),
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub enum PoolEscrowError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    InvalidAmount = 3,
    EscrowAlreadyExists = 4,
    EscrowNotFound = 5,
    InvalidStatus = 6,
    Unauthorized = 7,
}

/// Emitted when a cooperative locks funding for an invoice.
/// Topics: `["FundLocked", invoice_id]`, data: `amount`.
#[contractevent(topics = ["FundLocked"], data_format = "single-value")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FundLocked {
    #[topic]
    pub invoice_id: String,
    pub amount: i128,
}

/// Emitted when a locked escrow is released to the supplier.
/// Topics: `["InvoiceReleased", invoice_id]`, data: `amount`.
#[contractevent(topics = ["InvoiceReleased"], data_format = "single-value")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvoiceReleased {
    #[topic]
    pub invoice_id: String,
    pub amount: i128,
}

/// Emitted when a locked escrow is refunded to the funder.
/// Topics: `["EscrowRefunded", invoice_id]`, data: `amount`.
#[contractevent(topics = ["EscrowRefunded"], data_format = "single-value")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowRefunded {
    #[topic]
    pub invoice_id: String,
    pub amount: i128,
}

/// Emitted when a repayment is posted against a released escrow.
/// Topics: `["RepaymentPosted", invoice_id]`, data: `amount`.
#[contractevent(topics = ["RepaymentPosted"], data_format = "single-value")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepaymentPosted {
    #[topic]
    pub invoice_id: String,
    pub amount: i128,
}

// TTL (in ledgers) for persistent escrow records. ~30 days at 5s/ledger.
// Bump whenever a record is read or written so active escrows are never archived.
const ESCROW_TTL_THRESHOLD: u32 = 120_960; // ~7 days
const ESCROW_TTL_EXTEND_TO: u32 = 518_400; // ~30 days

#[contract]
pub struct PoolEscrowContract;

#[contractimpl]
impl PoolEscrowContract {
    /// One-time setup. Records the admin and requires the admin to authorize
    /// the call so it cannot be initialized on someone else's behalf.
    pub fn initialize(env: Env, admin: Address) -> Result<(), PoolEscrowError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(PoolEscrowError::AlreadyInitialized);
        }

        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        Ok(())
    }

    /// Lock cooperative funding for a single invoice. Only the funder can
    /// authorize the lock, the amount must be positive, and each invoice can
    /// only be locked once.
    pub fn lock_funding(
        env: Env,
        funder: Address,
        invoice_id: String,
        warung: Address,
        supplier: Address,
        cooperative: Address,
        amount: i128,
    ) -> Result<EscrowRecord, PoolEscrowError> {
        Self::require_initialized(&env)?;
        funder.require_auth();

        if amount <= 0 {
            return Err(PoolEscrowError::InvalidAmount);
        }

        let key = DataKey::Invoice(invoice_id.clone());
        if env.storage().persistent().has(&key) {
            return Err(PoolEscrowError::EscrowAlreadyExists);
        }

        let record = EscrowRecord {
            invoice_id: invoice_id.clone(),
            funder,
            warung,
            supplier,
            cooperative,
            amount,
            repaid_amount: 0,
            status: EscrowStatus::Locked,
            locked_at: env.ledger().timestamp(),
            closed_at: 0,
        };

        Self::write_record(&env, &key, &record);
        FundLocked { invoice_id, amount }.publish(&env);
        Ok(record)
    }

    /// Release a locked escrow to the supplier. Only the cooperative on the
    /// record can authorize it, and only while the escrow is still `Locked`.
    pub fn release_funding(
        env: Env,
        cooperative: Address,
        invoice_id: String,
    ) -> Result<EscrowRecord, PoolEscrowError> {
        let key = DataKey::Invoice(invoice_id.clone());
        let mut record = Self::read_record(&env, &key)?;

        cooperative.require_auth();
        if record.cooperative != cooperative {
            return Err(PoolEscrowError::Unauthorized);
        }

        if record.status != EscrowStatus::Locked {
            return Err(PoolEscrowError::InvalidStatus);
        }

        record.status = EscrowStatus::Released;
        record.closed_at = env.ledger().timestamp();
        Self::write_record(&env, &key, &record);
        InvoiceReleased {
            invoice_id,
            amount: record.amount,
        }
        .publish(&env);
        Ok(record)
    }

    /// Refund a locked escrow back to the funder. Same authorization rules as
    /// release: only the cooperative on the record, only while `Locked`.
    pub fn refund_funding(
        env: Env,
        cooperative: Address,
        invoice_id: String,
    ) -> Result<EscrowRecord, PoolEscrowError> {
        let key = DataKey::Invoice(invoice_id.clone());
        let mut record = Self::read_record(&env, &key)?;

        cooperative.require_auth();
        if record.cooperative != cooperative {
            return Err(PoolEscrowError::Unauthorized);
        }

        if record.status != EscrowStatus::Locked {
            return Err(PoolEscrowError::InvalidStatus);
        }

        record.status = EscrowStatus::Refunded;
        record.closed_at = env.ledger().timestamp();
        Self::write_record(&env, &key, &record);
        EscrowRefunded {
            invoice_id,
            amount: record.amount,
        }
        .publish(&env);
        Ok(record)
    }

    /// Record a repayment against a released escrow. The payer must authorize,
    /// the amount must be positive, and the escrow must already be `Released`.
    pub fn post_repayment(
        env: Env,
        payer: Address,
        invoice_id: String,
        amount: i128,
    ) -> Result<EscrowRecord, PoolEscrowError> {
        payer.require_auth();

        if amount <= 0 {
            return Err(PoolEscrowError::InvalidAmount);
        }

        let key = DataKey::Invoice(invoice_id.clone());
        let mut record = Self::read_record(&env, &key)?;

        if record.status != EscrowStatus::Released {
            return Err(PoolEscrowError::InvalidStatus);
        }

        record.repaid_amount = record
            .repaid_amount
            .checked_add(amount)
            .ok_or(PoolEscrowError::InvalidAmount)?;
        Self::write_record(&env, &key, &record);
        RepaymentPosted { invoice_id, amount }.publish(&env);
        Ok(record)
    }

    pub fn get_escrow(env: Env, invoice_id: String) -> Result<EscrowRecord, PoolEscrowError> {
        let key = DataKey::Invoice(invoice_id);
        Self::read_record(&env, &key)
    }

    pub fn get_admin(env: Env) -> Result<Address, PoolEscrowError> {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(PoolEscrowError::NotInitialized)
    }

    fn require_initialized(env: &Env) -> Result<(), PoolEscrowError> {
        if env.storage().instance().has(&DataKey::Admin) {
            Ok(())
        } else {
            Err(PoolEscrowError::NotInitialized)
        }
    }

    fn read_record(env: &Env, key: &DataKey) -> Result<EscrowRecord, PoolEscrowError> {
        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(key)
            .ok_or(PoolEscrowError::EscrowNotFound)?;
        env.storage()
            .persistent()
            .extend_ttl(key, ESCROW_TTL_THRESHOLD, ESCROW_TTL_EXTEND_TO);
        Ok(record)
    }

    fn write_record(env: &Env, key: &DataKey, record: &EscrowRecord) {
        env.storage().persistent().set(key, record);
        env.storage()
            .persistent()
            .extend_ttl(key, ESCROW_TTL_THRESHOLD, ESCROW_TTL_EXTEND_TO);
    }
}
