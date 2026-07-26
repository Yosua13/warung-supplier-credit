#![cfg(test)]

use crate::{
    EscrowStatus, PoolEscrowContract, PoolEscrowContractClient, PoolEscrowError,
};
use soroban_sdk::{
    testutils::{Address as _, Events, Ledger},
    vec, Address, Env, IntoVal, String, Symbol, Val, Vec,
};

struct TestSetup {
    env: Env,
    client: PoolEscrowContractClient<'static>,
    admin: Address,
    funder: Address,
    warung: Address,
    supplier: Address,
    cooperative: Address,
}

fn setup() -> TestSetup {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PoolEscrowContract, ());
    let client = PoolEscrowContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let funder = Address::generate(&env);
    let warung = Address::generate(&env);
    let supplier = Address::generate(&env);
    let cooperative = Address::generate(&env);

    client.initialize(&admin);

    TestSetup {
        env,
        client,
        admin,
        funder,
        warung,
        supplier,
        cooperative,
    }
}

fn invoice(env: &Env, id: &str) -> String {
    String::from_str(env, id)
}

// ---------------------------------------------------------------------------
// initialize
// ---------------------------------------------------------------------------

#[test]
fn test_initialize_once() {
    let s = setup();
    // A second initialize must fail with AlreadyInitialized.
    let res = s.client.try_initialize(&s.admin);
    assert_eq!(res, Err(Ok(PoolEscrowError::AlreadyInitialized)));
}

// ---------------------------------------------------------------------------
// lock_funding — happy path
// ---------------------------------------------------------------------------

#[test]
fn test_lock_funding_success() {
    let s = setup();
    let id = invoice(&s.env, "INV-001");

    s.env.ledger().set_timestamp(1_000);

    let record = s.client.lock_funding(
        &s.funder,
        &id,
        &s.warung,
        &s.supplier,
        &s.cooperative,
        &500_000i128,
    );

    // A FundLocked event was emitted with the correct topic and amount.
    // Assert immediately: each subsequent contract invocation resets the
    // test event buffer.
    let topics: Vec<Val> = (Symbol::new(&s.env, "FundLocked"), id.clone()).into_val(&s.env);
    let data: Val = 500_000i128.into_val(&s.env);
    let expected = vec![&s.env, (s.client.address.clone(), topics, data)];
    assert_eq!(s.env.events().all(), expected);

    assert_eq!(record.invoice_id, id);
    assert_eq!(record.funder, s.funder);
    assert_eq!(record.amount, 500_000);
    assert_eq!(record.repaid_amount, 0);
    assert_eq!(record.status, EscrowStatus::Locked);
    assert_eq!(record.locked_at, 1_000);
    assert_eq!(record.closed_at, 0);

    // Stored record is retrievable.
    let fetched = s.client.get_escrow(&id);
    assert_eq!(fetched, record);
}

// ---------------------------------------------------------------------------
// AC: same invoice cannot be locked twice
// ---------------------------------------------------------------------------

#[test]
fn test_lock_funding_duplicate_fails() {
    let s = setup();
    let id = invoice(&s.env, "INV-DUP");

    s.client.lock_funding(
        &s.funder,
        &id,
        &s.warung,
        &s.supplier,
        &s.cooperative,
        &100i128,
    );

    let res = s.client.try_lock_funding(
        &s.funder,
        &id,
        &s.warung,
        &s.supplier,
        &s.cooperative,
        &100i128,
    );
    assert_eq!(res, Err(Ok(PoolEscrowError::EscrowAlreadyExists)));
}

// ---------------------------------------------------------------------------
// AC: invalid amount rejected
// ---------------------------------------------------------------------------

#[test]
fn test_lock_funding_zero_amount_fails() {
    let s = setup();
    let id = invoice(&s.env, "INV-ZERO");
    let res = s.client.try_lock_funding(
        &s.funder,
        &id,
        &s.warung,
        &s.supplier,
        &s.cooperative,
        &0i128,
    );
    assert_eq!(res, Err(Ok(PoolEscrowError::InvalidAmount)));
}

#[test]
fn test_lock_funding_negative_amount_fails() {
    let s = setup();
    let id = invoice(&s.env, "INV-NEG");
    let res = s.client.try_lock_funding(
        &s.funder,
        &id,
        &s.warung,
        &s.supplier,
        &s.cooperative,
        &-1i128,
    );
    assert_eq!(res, Err(Ok(PoolEscrowError::InvalidAmount)));
}

// ---------------------------------------------------------------------------
// AC: unauthorized invocation fails (funder did not authorize)
// ---------------------------------------------------------------------------

#[test]
fn test_lock_funding_unauthorized_fails() {
    let env = Env::default();
    let contract_id = env.register(PoolEscrowContract, ());
    let client = PoolEscrowContractClient::new(&env, &contract_id);

    let funder = Address::generate(&env);
    let warung = Address::generate(&env);
    let supplier = Address::generate(&env);
    let cooperative = Address::generate(&env);
    let id = invoice(&env, "INV-NOAUTH");

    // No auths mocked at all -> funder.require_auth() must fail.
    env.mock_auths(&[]);
    let res = client.try_lock_funding(
        &funder,
        &id,
        &warung,
        &supplier,
        &cooperative,
        &100i128,
    );
    assert!(res.is_err());
}

// ---------------------------------------------------------------------------
// release_funding
// ---------------------------------------------------------------------------

#[test]
fn test_release_funding_success() {
    let s = setup();
    let id = invoice(&s.env, "INV-REL");

    s.client.lock_funding(
        &s.funder,
        &id,
        &s.warung,
        &s.supplier,
        &s.cooperative,
        &777i128,
    );

    s.env.ledger().set_timestamp(2_000);
    let record = s.client.release_funding(&s.cooperative, &id);

    assert_eq!(record.status, EscrowStatus::Released);
    assert_eq!(record.closed_at, 2_000);
}

#[test]
fn test_release_twice_fails() {
    let s = setup();
    let id = invoice(&s.env, "INV-REL2");

    s.client.lock_funding(
        &s.funder,
        &id,
        &s.warung,
        &s.supplier,
        &s.cooperative,
        &10i128,
    );
    s.client.release_funding(&s.cooperative, &id);

    // Already released -> InvalidStatus.
    let res = s.client.try_release_funding(&s.cooperative, &id);
    assert_eq!(res, Err(Ok(PoolEscrowError::InvalidStatus)));
}

#[test]
fn test_release_wrong_cooperative_fails() {
    let s = setup();
    let id = invoice(&s.env, "INV-WRONGCOOP");

    s.client.lock_funding(
        &s.funder,
        &id,
        &s.warung,
        &s.supplier,
        &s.cooperative,
        &10i128,
    );

    // A different cooperative than the one on the record cannot release,
    // even though mock_all_auths approves the signature.
    let stranger = Address::generate(&s.env);
    let res = s.client.try_release_funding(&stranger, &id);
    assert_eq!(res, Err(Ok(PoolEscrowError::Unauthorized)));
}

#[test]
fn test_lock_funding_before_initialize_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(PoolEscrowContract, ());
    let client = PoolEscrowContractClient::new(&env, &contract_id);

    let funder = Address::generate(&env);
    let warung = Address::generate(&env);
    let supplier = Address::generate(&env);
    let cooperative = Address::generate(&env);
    let id = invoice(&env, "INV-NOINIT");

    // Contract was never initialized.
    let res = client.try_lock_funding(
        &funder,
        &id,
        &warung,
        &supplier,
        &cooperative,
        &100i128,
    );
    assert_eq!(res, Err(Ok(PoolEscrowError::NotInitialized)));
}

#[test]
fn test_release_missing_invoice_fails() {
    let s = setup();
    let id = invoice(&s.env, "INV-MISSING");
    let res = s.client.try_release_funding(&s.cooperative, &id);
    assert_eq!(res, Err(Ok(PoolEscrowError::EscrowNotFound)));
}

// ---------------------------------------------------------------------------
// refund_funding
// ---------------------------------------------------------------------------

#[test]
fn test_refund_funding_success() {
    let s = setup();
    let id = invoice(&s.env, "INV-REF");

    s.client.lock_funding(
        &s.funder,
        &id,
        &s.warung,
        &s.supplier,
        &s.cooperative,
        &42i128,
    );
    let record = s.client.refund_funding(&s.cooperative, &id);
    assert_eq!(record.status, EscrowStatus::Refunded);
}

#[test]
fn test_refund_after_release_fails() {
    let s = setup();
    let id = invoice(&s.env, "INV-REF2");

    s.client.lock_funding(
        &s.funder,
        &id,
        &s.warung,
        &s.supplier,
        &s.cooperative,
        &42i128,
    );
    s.client.release_funding(&s.cooperative, &id);

    let res = s.client.try_refund_funding(&s.cooperative, &id);
    assert_eq!(res, Err(Ok(PoolEscrowError::InvalidStatus)));
}

// ---------------------------------------------------------------------------
// post_repayment
// ---------------------------------------------------------------------------

#[test]
fn test_post_repayment_success() {
    let s = setup();
    let id = invoice(&s.env, "INV-REPAY");

    s.client.lock_funding(
        &s.funder,
        &id,
        &s.warung,
        &s.supplier,
        &s.cooperative,
        &1_000i128,
    );
    s.client.release_funding(&s.cooperative, &id);

    let r1 = s.client.post_repayment(&s.warung, &id, &400i128);
    assert_eq!(r1.repaid_amount, 400);
    let r2 = s.client.post_repayment(&s.warung, &id, &600i128);
    assert_eq!(r2.repaid_amount, 1_000);
    assert_eq!(r2.status, EscrowStatus::Released);
}

#[test]
fn test_post_repayment_before_release_fails() {
    let s = setup();
    let id = invoice(&s.env, "INV-REPAY2");

    s.client.lock_funding(
        &s.funder,
        &id,
        &s.warung,
        &s.supplier,
        &s.cooperative,
        &1_000i128,
    );

    // Still Locked, not Released -> InvalidStatus.
    let res = s.client.try_post_repayment(&s.warung, &id, &100i128);
    assert_eq!(res, Err(Ok(PoolEscrowError::InvalidStatus)));
}

#[test]
fn test_post_repayment_invalid_amount_fails() {
    let s = setup();
    let id = invoice(&s.env, "INV-REPAY3");

    s.client.lock_funding(
        &s.funder,
        &id,
        &s.warung,
        &s.supplier,
        &s.cooperative,
        &1_000i128,
    );
    s.client.release_funding(&s.cooperative, &id);

    let res = s.client.try_post_repayment(&s.warung, &id, &0i128);
    assert_eq!(res, Err(Ok(PoolEscrowError::InvalidAmount)));
}

// ---------------------------------------------------------------------------
// get_escrow on missing invoice
// ---------------------------------------------------------------------------

#[test]
fn test_get_escrow_not_found() {
    let s = setup();
    let id = invoice(&s.env, "INV-NONE");
    let res = s.client.try_get_escrow(&id);
    assert_eq!(res, Err(Ok(PoolEscrowError::EscrowNotFound)));
}
