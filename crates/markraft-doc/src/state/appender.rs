//! Transaction appenders: reacting to a finished transaction with another one.
//!
//! An appender does not change the transaction it reacts to — that is what a
//! [`transaction_extender`](super::transaction_extender) is for. It produces a
//! *separate* transaction, applied on the state the trigger left behind, so a
//! view layer sees both steps and an undo folds them together.

use super::StateError;
use super::annotation;
use super::filters;
use super::transaction::{Transaction, TransactionSpec, resolve};

/// The largest number of transactions one dispatch may append.
///
/// The chain stops there and the last appended transaction is annotated
/// [`appenders_diverged`](super::appenders_diverged), whether or not the
/// appenders would have stopped on their own.
pub const MAX_APPENDED_TRANSACTIONS: usize = 8;

/// Run the configured appenders over `primary` and whatever they produce.
///
/// Every appended transaction is resolved against the state the previous one
/// left behind, so the returned list can be applied in order. Appenders run in
/// reverse configuration order, like extenders, and all of them see the same
/// trigger; what they produce becomes the trigger of the next round.
pub(crate) fn append(primary: Transaction) -> Result<Vec<Transaction>, StateError> {
    let mut out = vec![primary];
    let mut produced = 0usize;
    loop {
        let trigger = out.last().expect("the primary transaction").clone();
        let mut state = trigger.state().clone();
        let appenders = state.facet(filters::transaction_appender()).clone();
        if appenders.is_empty() {
            return Ok(out);
        }
        let specs: Vec<TransactionSpec> = appenders
            .iter()
            .rev()
            .filter_map(|appender| appender(&trigger))
            .collect();
        if specs.is_empty() {
            return Ok(out);
        }
        // A transaction the history ignores must not become undoable by way of
        // something an appender added to it.
        let off_history = trigger.annotation(annotation::add_to_history()) == Some(&false);
        let info = filters::Appended {
            trigger_user_event: trigger.user_event_name().map(str::to_string),
            trigger_origin: trigger.annotation(annotation::origin()).cloned(),
            depth: 0,
        };
        for spec in specs {
            produced += 1;
            let cut = produced >= MAX_APPENDED_TRANSACTIONS;
            let mut spec = spec.annotate(filters::appended().of(filters::Appended {
                depth: produced,
                ..info.clone()
            }));
            if off_history {
                spec = spec.add_to_history(false);
            }
            if cut {
                spec = spec.annotate(filters::appenders_diverged().of(true));
            }
            let tr = resolve(&state, vec![spec], true, true)?;
            state = tr.state().clone();
            out.push(tr);
            if cut {
                return Ok(out);
            }
        }
    }
}
