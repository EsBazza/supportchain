#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger, LedgerInfo},
    token::{Client as TokenClient, StellarAssetClient},
    Address, Env, String,
};

const T0: u64 = 1_700_000_000;
const MONTHLY_AMOUNT: i128 = 6_000;
const DUE_DAY: u32 = 15;
const SECONDS_PER_MONTH: u64 = 30 * 24 * 60 * 60;

fn set_time(env: &Env, ts: u64) {
    env.ledger().set(LedgerInfo {
        timestamp: ts,
        protocol_version: 22,
        sequence_number: env.ledger().sequence(),
        network_id: Default::default(),
        base_reserve: 10,
        min_temp_entry_ttl: 1000,
        min_persistent_entry_ttl: 1000,
        max_entry_ttl: 6_312_000,
    });
}

struct Ctx {
    env: Env,
    court_admin: Address,
    token: Address,
    paying_parent: Address,
    custodial_parent: Address,
    client: SupportChainContractClient<'static>,
}

fn setup() -> Ctx {
    let env = Env::default();
    env.mock_all_auths();
    set_time(&env, T0);

    let token_admin = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(token_admin.clone())
        .address();

    let court_admin = Address::generate(&env);
    let paying_parent = Address::generate(&env);
    let custodial_parent = Address::generate(&env);

    let contract_id = env.register(SupportChainContract, ());
    let client = SupportChainContractClient::new(&env, &contract_id);

    client.initialize(&court_admin, &token);
    client.create_order(
        &court_admin,
        &paying_parent,
        &custodial_parent,
        &MONTHLY_AMOUNT,
        &DUE_DAY,
        &String::from_str(&env, "Maria's Child"),
        &String::from_str(&env, "FC-QC-2024-1234"),
    );

    Ctx {
        env,
        court_admin,
        token,
        paying_parent,
        custodial_parent,
        client,
    }
}

fn mint_usdc(ctx: &Ctx, to: &Address, amount: i128) {
    StellarAssetClient::new(&ctx.env, &ctx.token).mint(to, &amount);
}

fn payer_balance(ctx: &Ctx) -> i128 {
    TokenClient::new(&ctx.env, &ctx.token).balance(&ctx.paying_parent)
}

fn custodian_balance(ctx: &Ctx) -> i128 {
    TokenClient::new(&ctx.env, &ctx.token).balance(&ctx.custodial_parent)
}

fn advance_months(ctx: &Ctx, n: u64) {
    let current = ctx.env.ledger().timestamp();
    set_time(&ctx.env, current + n * SECONDS_PER_MONTH);
}

#[test]
fn test_execute_payment_success_end_to_end() {
    let ctx = setup();
    mint_usdc(&ctx, &ctx.paying_parent, 10_000);
    assert_eq!(payer_balance(&ctx), 10_000);
    advance_months(&ctx, 1);
    let result = ctx.client.execute_payment();
    assert_eq!(result, PaymentResult::Paid);
    assert_eq!(custodian_balance(&ctx), MONTHLY_AMOUNT);
    assert_eq!(payer_balance(&ctx), 10_000 - MONTHLY_AMOUNT);
    assert_eq!(ctx.client.get_compliance_score(), 10_000);
    let history = ctx.client.get_payment_history();
    assert_eq!(history.len(), 1);
    assert_eq!(history.get(0).unwrap().success, true);
    assert_eq!(history.get(0).unwrap().amount_paid, MONTHLY_AMOUNT);
    assert!(ctx.client.is_current_month_paid());
    assert_eq!(ctx.client.get_total_paid(), MONTHLY_AMOUNT);
    assert_eq!(ctx.client.get_payment_count(), 1);
}

#[test]
fn test_execute_payment_insufficient_balance_defaults() {
    let ctx = setup();
    mint_usdc(&ctx, &ctx.paying_parent, 2_000);
    advance_months(&ctx, 1);
    let result = ctx.client.execute_payment();
    assert_eq!(result, PaymentResult::Defaulted);
    assert_eq!(custodian_balance(&ctx), 0);
    assert_eq!(payer_balance(&ctx), 2_000);
    assert_eq!(ctx.client.get_compliance_score(), 0);
    let history = ctx.client.get_payment_history();
    assert_eq!(history.len(), 1);
    let record = history.get(0).unwrap();
    assert_eq!(record.success, false);
    assert_eq!(record.amount_paid, 0);
    assert_eq!(record.payer_balance_at_attempt, 2_000);
}

#[test]
fn test_create_order_stores_correct_state() {
    let ctx = setup();
    let order = ctx.client.get_order();
    assert_eq!(order.paying_parent, ctx.paying_parent);
    assert_eq!(order.custodial_parent, ctx.custodial_parent);
    assert_eq!(order.monthly_amount, MONTHLY_AMOUNT);
    assert_eq!(order.due_day, DUE_DAY);
    assert_eq!(order.active, true);
    assert_eq!(order.created_at, T0);
    let config = ctx.client.get_config();
    assert_eq!(config.court_admin, ctx.court_admin);
    assert_eq!(config.token, ctx.token);
}

#[test]
#[should_panic(expected = "payment already executed for this month")]
fn test_cannot_execute_same_month_twice() {
    let ctx = setup();
    mint_usdc(&ctx, &ctx.paying_parent, 20_000);
    advance_months(&ctx, 1);
    let result1 = ctx.client.execute_payment();
    assert_eq!(result1, PaymentResult::Paid);
    ctx.client.execute_payment();
}

#[test]
fn test_compliance_score_reflects_mixed_history() {
    let ctx = setup();
    mint_usdc(&ctx, &ctx.paying_parent, 10_000);
    advance_months(&ctx, 1);
    ctx.client.execute_payment();
    advance_months(&ctx, 1);
    ctx.client.execute_payment();
    mint_usdc(&ctx, &ctx.paying_parent, 20_000);
    advance_months(&ctx, 1);
    ctx.client.execute_payment();
    let expected_score = 2 * BPS_DENOM / 3; // 6,666
    assert_eq!(ctx.client.get_compliance_score(), expected_score);
    assert_eq!(ctx.client.get_payment_history().len(), 3);
    assert_eq!(ctx.client.get_payment_count(), 2);
    assert_eq!(ctx.client.get_total_paid(), 2 * MONTHLY_AMOUNT);
}