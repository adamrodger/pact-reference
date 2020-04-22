//! Handles wrapping Rust models

use pact_matching::models::{Pact, Consumer, Provider, Interaction};
use lazy_static::*;
use std::sync::Mutex;
use std::cell::RefCell;

lazy_static! {
  static ref PACT_HANDLES: Mutex<Vec<RefCell<Pact>>> = Mutex::new(vec![]);
}

#[repr(C)]
#[derive(Debug, Clone)]
/// Wraps a Pact model struct
pub struct PactHandle {
  pub pact: usize
}

#[repr(C)]
#[derive(Debug, Clone)]
/// Wraps a Pact model struct
pub struct InteractionHandle {
  pub pact: usize,
  pub interaction: usize
}

#[repr(C)]
#[derive(Debug, Clone)]
/// Result enum for returning the Pact model
pub enum PactResult {
  /// OK result containing the handle
  Ok(PactHandle),
  /// Error result containing an error code
  Err(usize)
}

impl PactHandle {
  /// Creates a new handle to a Pact model
  pub fn new(consumer: &str, provider: &str) -> Self {
    let mut handles = PACT_HANDLES.lock().unwrap();
    handles.push(RefCell::new(Pact {
      consumer: Consumer { name: consumer.clone().to_string() },
      provider: Provider { name: provider.clone().to_string() },
      .. Pact::default()
    }));
    PactHandle {
      pact: handles.len()
    }
  }

  /// Invokes the closure with the inner Pact model
  pub fn with_pact<R>(&self, f: &dyn Fn(usize, &mut Pact) -> R) -> Option<R> {
    let mut handles = PACT_HANDLES.lock().unwrap();
    handles.get_mut(self.pact - 1).map(|inner| f(self.pact - 1, &mut inner.borrow_mut()))
  }
}

impl InteractionHandle {
  /// Creates a new handle to an Interaction
  pub fn new(pact: PactHandle, interaction: usize) -> InteractionHandle {
    InteractionHandle {
      pact: pact.pact,
      interaction
    }
  }

  /// Invokes the closure with the inner Pact model
  pub fn with_pact<R>(&self, f: &dyn Fn(usize, &mut Pact) -> R) -> Option<R> {
    let mut handles = PACT_HANDLES.lock().unwrap();
    handles.get_mut(self.pact - 1).map(|inner| f(self.pact - 1, &mut inner.borrow_mut()))
  }

  /// Invokes the closure with the inner Interaction model
  pub fn with_interaction<R>(&self, f: &dyn Fn(usize, &mut Interaction) -> R) -> Option<R> {
    let mut handles = PACT_HANDLES.lock().unwrap();
    handles.get_mut(self.pact - 1).map(|inner| {
      match inner.borrow_mut().interactions.get_mut(self.interaction - 1) {
        Some(inner_i) => Some(f(self.interaction - 1, inner_i)),
        None => None
      }
    }).flatten()
  }
}