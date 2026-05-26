#![no_std]

use soroban_sdk::{
    contract, contractimpl, contracttype, token, Address, Env, String, Symbol, Vec,
};

// ─────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────

/// Symbol key for initialization guard.
const INITIALIZED: Symbol = Symbol::new(&Env::default(), "INIT"); // placeholder, will be created per env
// Actually we cannot use Env::default() – we'll create inside functions.
// Better: use a helper function to get the symbol.
// For simplicity, we'll use Symbol::from_str() inside functions.
// But for const, we use a workaround: define as &str and convert later.
// We'll adjust: Instead of const, we'll inline Symbol::new.

// Number of basis points in 100% (for compliance score calculations).
const BPS_DENOM: u64 = 10_000;

// ─────────────────────────────────────────────
// Storage Keys
// ─────────────────────────────────────────────

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Config,
    SupportOrder,
    PaymentHistory,
    ComplianceScore,
    NextPaymentId,
    LastExecutedMonth,
}

// ─────────────────────────────────────────────
// Types
// ─────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug)]
pub struct Config {
    pub court_admin: Address,
    pub token: Address,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct SupportOrder {
    pub paying_parent: Address,
    pub custodial_parent: Address,
    pub monthly_amount: i128,
    pub due_day: u32,
    pub child_name: String,
    pub case_number: String,
    pub created_at: u64,
    pub active: bool,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct PaymentRecord {
    pub id: u64,
    pub period: u64,
    pub amount_paid: i128,
    pub success: bool,
    pub timestamp: u64,
    pub payer_balance_at_attempt: i128,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum PaymentResult {
    Paid,
    Defaulted,
}

// ─────────────────────────────────────────────
// Contract
// ─────────────────────────────────────────────

#[contract]
pub struct SupportChainContract;

#[contractimpl]
impl SupportChainContract {
    // ── Initialisation ───────────────────────

    pub fn initialize(env: Env, court_admin: Address, token: Address) {
        let init_key = Symbol::new(&env, "INIT");
        if env.storage().instance().has(&init_key) {
            panic!("already initialized");
        }

        env.storage().instance().set(&init_key, &true);
        env.storage()
            .instance()
            .set(&DataKey::Config, &Config { court_admin, token });
        env.storage()
            .instance()
            .set(&DataKey::ComplianceScore, &10_000u64);
        env.storage()
            .instance()
            .set(&DataKey::NextPaymentId, &1u64);
        env.storage()
            .instance()
            .set(&DataKey::LastExecutedMonth, &0u64);
        env.storage()
            .instance()
            .set(&DataKey::PaymentHistory, &Vec::<PaymentRecord>::new(&env));
    }

    // ── Court Admin: Create Support Order ────

    pub fn create_order(
        env: Env,
        court_admin: Address,
        paying_parent: Address,
        custodial_parent: Address,
        monthly_amount: i128,
        due_day: u32,
        child_name: String,
        case_number: String,
    ) {
        court_admin.require_auth();
        let config = Self::load_config(&env);
        if config.court_admin != court_admin {
            panic!("caller is not the court admin");
        }
        if monthly_amount <= 0 {
            panic!("monthly amount must be positive");
        }
        if due_day < 1 || due_day > 28 {
            panic!("due day must be between 1 and 28");
        }
        if child_name.is_empty() || case_number.is_empty() {
            panic!("child name and case number cannot be empty");
        }

        let order = SupportOrder {
            paying_parent,
            custodial_parent,
            monthly_amount,
            due_day,
            child_name,
            case_number,
            created_at: env.ledger().timestamp(),
            active: true,
        };

        env.storage().instance().set(&DataKey::SupportOrder, &order);
    }

    // ── Execute Monthly Payment ──────────────

    pub fn execute_payment(env: Env) -> PaymentResult {
        let order: SupportOrder = env
            .storage()
            .instance()
            .get(&DataKey::SupportOrder)
            .expect("no support order exists");

        if !order.active {
            panic!("support order is not active");
        }

        let ledger_ts = env.ledger().timestamp();
        let current_month = Self::timestamp_to_month(&env, ledger_ts);

        let last_month: u64 = env
            .storage()
            .instance()
            .get(&DataKey::LastExecutedMonth)
            .unwrap_or(0);

        if current_month <= last_month {
            panic!("payment already executed for this month");
        }

        let config = Self::load_config(&env);
        let token_client = token::Client::new(&env, &config.token);
        let payer_balance = token_client.balance(&order.paying_parent);

        let (amount_paid, success) = if payer_balance >= order.monthly_amount {
            token_client.transfer(
                &order.paying_parent,
                &order.custodial_parent,
                &order.monthly_amount,
            );
            (order.monthly_amount, true)
        } else {
            (0i128, false)
        };

        let next_id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::NextPaymentId)
            .unwrap();

        let record = PaymentRecord {
            id: next_id,
            period: current_month,
            amount_paid,
            success,
            timestamp: ledger_ts,
            payer_balance_at_attempt: payer_balance,
        };

        let mut history: Vec<PaymentRecord> = env
            .storage()
            .instance()
            .get(&DataKey::PaymentHistory)
            .unwrap_or(Vec::new(&env));
        history.push_back(record);
        env.storage()
            .instance()
            .set(&DataKey::PaymentHistory, &history);

        Self::update_compliance_score(&env);

        env.storage()
            .instance()
            .set(&DataKey::LastExecutedMonth, &current_month);
        env.storage()
            .instance()
            .set(&DataKey::NextPaymentId, &(next_id + 1));

        if success {
            PaymentResult::Paid
        } else {
            PaymentResult::Defaulted
        }
    }

    // ── Court Admin: Deactivate Order ────────

    pub fn deactivate_order(env: Env, court_admin: Address) {
        court_admin.require_auth();
        let config = Self::load_config(&env);
        if config.court_admin != court_admin {
            panic!("caller is not the court admin");
        }

        let mut order: SupportOrder = env
            .storage()
            .instance()
            .get(&DataKey::SupportOrder)
            .expect("no support order exists");

        order.active = false;
        env.storage().instance().set(&DataKey::SupportOrder, &order);
    }

    // ── Queries ──────────────────────────────

    pub fn get_config(env: Env) -> Config {
        Self::load_config(&env)
    }

    pub fn get_order(env: Env) -> SupportOrder {
        env.storage()
            .instance()
            .get(&DataKey::SupportOrder)
            .expect("no support order exists")
    }

    pub fn get_payment_history(env: Env) -> Vec<PaymentRecord> {
        env.storage()
            .instance()
            .get(&DataKey::PaymentHistory)
            .unwrap_or(Vec::new(&env))
    }

    pub fn get_compliance_score(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::ComplianceScore)
            .unwrap_or(0)
    }

    pub fn get_payment_count(env: Env) -> u64 {
        let history: Vec<PaymentRecord> = env
            .storage()
            .instance()
            .get(&DataKey::PaymentHistory)
            .unwrap_or(Vec::new(&env));
        let mut count: u64 = 0;
        for record in history.iter() {
            if record.success {
                count += 1;
            }
        }
        count
    }

    pub fn get_total_paid(env: Env) -> i128 {
        let history: Vec<PaymentRecord> = env
            .storage()
            .instance()
            .get(&DataKey::PaymentHistory)
            .unwrap_or(Vec::new(&env));
        let mut total: i128 = 0;
        for record in history.iter() {
            total += record.amount_paid;
        }
        total
    }

    pub fn is_current_month_paid(env: Env) -> bool {
        let last_month: u64 = env
            .storage()
            .instance()
            .get(&DataKey::LastExecutedMonth)
            .unwrap_or(0);
        let current_month = Self::timestamp_to_month(&env, env.ledger().timestamp());
        last_month >= current_month
    }

    // ── Internal Helpers ─────────────────────

    fn load_config(env: &Env) -> Config {
        env.storage()
            .instance()
            .get(&DataKey::Config)
            .expect("contract not initialized")
    }

    fn update_compliance_score(env: &Env) {
        let history: Vec<PaymentRecord> = env
            .storage()
            .instance()
            .get(&DataKey::PaymentHistory)
            .unwrap_or(Vec::new(env));

        if history.is_empty() {
            return;
        }

        let total = history.len() as u64;
        let mut successful: u64 = 0;
        for record in history.iter() {
            if record.success {
                successful += 1;
            }
        }

        let score = successful * BPS_DENOM / total;
        env.storage()
            .instance()
            .set(&DataKey::ComplianceScore, &score);
    }

    /// Simplified: 1 month = 30 days of seconds
    fn timestamp_to_month(env: &Env, ts: u64) -> u64 {
        let seconds_per_month: u64 = 30 * 24 * 60 * 60;
        ts / seconds_per_month
    }
}