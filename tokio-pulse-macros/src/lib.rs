//     ______   __  __     __         ______     ______
//    /\  == \ /\ \/\ \   /\ \       /\  ___\   /\  ___\
//    \ \  _-/ \ \ \_\ \  \ \ \____  \ \___  \  \ \  __\
//     \ \_\    \ \_____\  \ \_____\  \/\_____\  \ \_____\
//      \/_/     \/_____/   \/_____/   \/_____/   \/_____/
//
// Author: Colin MacRitchie / Ripple Group
//! Procedural macros for Tokio-Pulse preemption system
//!
//! This crate provides compile-time instrumentation macros for integrating
//! preemption budget management into async functions.

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::{
    parse_macro_input, parse_quote, Error, Expr, Ident, ItemFn, Lit, Meta,
    MetaNameValue, Result, Stmt, visit_mut::VisitMut, punctuated::Punctuated, Token
};

/// Configuration for preemption budget macro
#[derive(Debug, Clone)]
struct BudgetConfig {
    /// Initial budget allocation
    budget: Option<u32>,
    /// Enable CPU time-based deadlines
    deadline: Option<bool>,
    /// Budget check frequency (every N polls)
    check_interval: Option<u32>,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            budget: Some(2000), // DEFAULT_BUDGET from tokio-pulse
            deadline: Some(false),
            check_interval: Some(1),
        }
    }
}

impl BudgetConfig {
    /// Parse configuration from comma-separated meta items
    fn from_meta_list(meta_list: &Punctuated<Meta, Token![,]>) -> Result<Self> {
        let mut config = Self::default();

        for meta in meta_list {
            match meta {
                Meta::NameValue(MetaNameValue { path, value, .. })
                    if path.is_ident("budget") => {
                    match value {
                        Expr::Lit(expr_lit) => {
                            if let Lit::Int(int_lit) = &expr_lit.lit {
                                let budget_value = int_lit.base10_parse::<u32>()?;
                                if !(100..=10_000).contains(&budget_value) {
                                    return Err(Error::new_spanned(
                                        int_lit,
                                        "Budget must be between 100 and 10,000"
                                    ));
                                }
                                config.budget = Some(budget_value);
                            } else {
                                return Err(Error::new_spanned(
                                    expr_lit,
                                    "Budget must be an integer"
                                ));
                            }
                        }
                        _ => {
                            return Err(Error::new_spanned(
                                value,
                                "Budget value must be an integer literal"
                            ));
                        }
                    }
                }
                Meta::NameValue(MetaNameValue { path, value, .. })
                    if path.is_ident("deadline") => {
                    match value {
                        Expr::Lit(expr_lit) => {
                            if let Lit::Bool(bool_lit) = &expr_lit.lit {
                                config.deadline = Some(bool_lit.value);
                            } else {
                                return Err(Error::new_spanned(
                                    expr_lit,
                                    "Deadline must be a boolean"
                                ));
                            }
                        }
                        _ => {
                            return Err(Error::new_spanned(
                                value,
                                "Deadline value must be a boolean literal"
                            ));
                        }
                    }
                }
                Meta::NameValue(MetaNameValue { path, value, .. })
                    if path.is_ident("check_interval") => {
                    match value {
                        Expr::Lit(expr_lit) => {
                            if let Lit::Int(int_lit) = &expr_lit.lit {
                                let interval_value = int_lit.base10_parse::<u32>()?;
                                if interval_value == 0 {
                                    return Err(Error::new_spanned(
                                        int_lit,
                                        "Check interval must be greater than 0"
                                    ));
                                }
                                config.check_interval = Some(interval_value);
                            } else {
                                return Err(Error::new_spanned(
                                    expr_lit,
                                    "Check interval must be an integer"
                                ));
                            }
                        }
                        _ => {
                            return Err(Error::new_spanned(
                                value,
                                "Check interval value must be an integer literal"
                            ));
                        }
                    }
                }
                _ => {
                    return Err(Error::new_spanned(
                        meta,
                        "Invalid argument. Supported: budget, deadline, check_interval"
                    ));
                }
            }
        }

        Ok(config)
    }
}

/// Visitor that injects budget checks at await points
struct BudgetInjector {
    config: BudgetConfig,
    budget_ident: Ident,
    poll_count: u32,
}

impl BudgetInjector {
    fn new(config: BudgetConfig) -> Self {
        Self {
            config,
            budget_ident: Ident::new("__pulse_budget", Span::call_site()),
            poll_count: 0,
        }
    }

    /// Generate budget check expression
    fn generate_budget_check(&self) -> Expr {
        let budget_ident = &self.budget_ident;
        parse_quote! {
            if #budget_ident.consume() {
                ::tokio::task::yield_now().await;
                #budget_ident.reset(#budget_ident.remaining().max(100));
            }
        }
    }
}

impl VisitMut for BudgetInjector {
    fn visit_block_mut(&mut self, block: &mut syn::Block) {
        // Transform all statements in the block
        for stmt in &mut block.stmts {
            self.visit_stmt_mut(stmt);
        }

        // Add budget check statements after await expressions
        let mut new_stmts = Vec::new();
        for stmt in &block.stmts {
            new_stmts.push(stmt.clone());

            // Check if this statement contains an await and should trigger budget check
            if self.stmt_contains_await(stmt) {
                self.poll_count += 1;
                if self.poll_count % self.config.check_interval.unwrap_or(1) == 0 {
                    let budget_check = self.generate_budget_check();
                    new_stmts.push(Stmt::Expr(budget_check, None));
                }
            }
        }

        block.stmts = new_stmts;
    }
}

impl BudgetInjector {
    fn stmt_contains_await(&self, stmt: &Stmt) -> bool {
        match stmt {
            Stmt::Expr(expr, _) => self.expr_contains_await(expr),
            Stmt::Local(local) => {
                if let Some(init) = &local.init {
                    self.expr_contains_await(&init.expr)
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    fn expr_contains_await(&self, expr: &Expr) -> bool {
        match expr {
            Expr::Await(_) => true,
            Expr::Block(block) => block.block.stmts.iter().any(|stmt| self.stmt_contains_await(stmt)),
            Expr::Call(call) => self.expr_contains_await(&call.func) ||
                               call.args.iter().any(|arg| self.expr_contains_await(arg)),
            Expr::MethodCall(method) => self.expr_contains_await(&method.receiver) ||
                                       method.args.iter().any(|arg| self.expr_contains_await(arg)),
            _ => false,
        }
    }
}

/// Attributes for `#[preemption_budget]` macro
///
/// Instruments async functions with preemption budget management.
/// Budget is checked at every await point to prevent task starvation.
///
/// # Arguments
///
/// * `budget` - Initial operation budget (default: 2000, range: 100-10000)
/// * `deadline` - Enable CPU time deadlines (default: false)
/// * `check_interval` - Check budget every N await points (default: 1)
///
/// # Examples
///
/// ```rust
/// #[preemption_budget]
/// async fn process_data() {
///     // Budget automatically managed at await points
///     let data = fetch_data().await;
///     process(data).await;
/// }
///
/// #[preemption_budget(budget = 1000, check_interval = 2)]
/// async fn high_frequency_task() {
///     // Custom budget with less frequent checks
///     loop {
///         work().await;
///         more_work().await; // Budget checked here (every 2nd await)
///     }
/// }
/// ```
#[proc_macro_attribute]
pub fn preemption_budget(args: TokenStream, input: TokenStream) -> TokenStream {
    let args = parse_macro_input!(args with Punctuated::<Meta, Token![,]>::parse_terminated);
    let input_fn = parse_macro_input!(input as ItemFn);

    match expand_preemption_budget(args, input_fn) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// Main expansion logic for preemption_budget macro
fn expand_preemption_budget(args: Punctuated<Meta, Token![,]>, mut input_fn: ItemFn) -> Result<TokenStream2> {
    // Validate function is async
    if input_fn.sig.asyncness.is_none() {
        return Err(Error::new_spanned(
            input_fn.sig.fn_token,
            "#[preemption_budget] can only be applied to async functions"
        ));
    }

    // Parse configuration
    let config = BudgetConfig::from_meta_list(&args)?;

    // Generate budget initialization
    let budget_value = config.budget.unwrap_or(2000);
    let budget_ident = Ident::new("__pulse_budget", Span::call_site());

    let budget_init: Stmt = parse_quote! {
        let #budget_ident = ::tokio_pulse::TaskBudget::new(#budget_value);
    };

    // Create visitor and transform function body
    let mut injector = BudgetInjector::new(config);

    // Insert budget initialization at start of function
    if let Some(_first_stmt) = input_fn.block.stmts.first() {
        input_fn.block.stmts.insert(0, budget_init);
    } else {
        input_fn.block.stmts.push(budget_init);
    }

    // Transform await points
    injector.visit_block_mut(&mut input_fn.block);

    Ok(quote! { #input_fn })
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    #[test]
    fn test_budget_config_parsing() {
        let meta_list: Punctuated<Meta, Token![,]> = parse_quote!(budget = 1500, deadline = true);
        let config = BudgetConfig::from_meta_list(&meta_list).unwrap();

        assert_eq!(config.budget, Some(1500));
        assert_eq!(config.deadline, Some(true));
    }

    #[test]
    fn test_invalid_budget_range() {
        let meta_list: Punctuated<Meta, Token![,]> = parse_quote!(budget = 50);
        let result = BudgetConfig::from_meta_list(&meta_list);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Budget must be between"));
    }

    #[test]
    fn test_budget_injection() {
        let input_fn: ItemFn = parse_quote! {
            async fn test_fn() {
                let x = some_async_call().await;
                another_call().await;
            }
        };

        let empty_args = Punctuated::new();
        let result = expand_preemption_budget(empty_args, input_fn);
        assert!(result.is_ok());

        let output = result.unwrap().to_string();
        assert!(output.contains("__pulse_budget"));
        assert!(output.contains("TaskBudget::new"));
    }
}