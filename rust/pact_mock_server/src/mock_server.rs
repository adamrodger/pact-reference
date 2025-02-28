//!
//! This module defines the external interface for controlling one particular
//! instance of a mock server.
//!

use std::cell::RefCell;
use std::ffi::CString;
use std::ops::DerefMut;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use log::*;
use pact_plugin_driver::plugin_manager::drop_plugin_access;
use pact_plugin_driver::plugin_models::{PluginDependency, PluginDependencyType};
use rustls::ServerConfig;
use serde::{Deserialize, Serialize};
use serde_json::json;

use pact_models::pact::{Pact, write_pact};
use pact_models::PactSpecification;
use pact_models::sync_pact::RequestResponsePact;
use pact_models::v4::http_parts::HttpRequest;

use crate::hyper_server;
use crate::matching::MatchResult;

/// Mock server configuration
#[derive(Debug, Default, Clone)]
pub struct MockServerConfig {
  /// If CORS Pre-Flight requests should be responded to
  pub cors_preflight: bool,
  /// Pact specification to use
  pub pact_specification: PactSpecification
}

/// Mock server scheme
#[derive(Debug, Clone)]
pub enum MockServerScheme {
  /// HTTP
  HTTP,
  /// HTTPS
  HTTPS
}

impl Default for MockServerScheme {
  fn default() -> Self {
    MockServerScheme::HTTP
  }
}

impl ToString for MockServerScheme {
  fn to_string(&self) -> String {
    match self {
      MockServerScheme::HTTP => "http".into(),
      MockServerScheme::HTTPS => "https".into()
    }
  }
}

/// Metrics for the mock server
#[derive(Debug, Default, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct MockServerMetrics {
  /// Total requests
  pub requests: usize
}

/// Struct to represent the "foreground" part of mock server
#[derive(Debug)]
pub struct MockServer {
  /// Mock server unique ID
  pub id: String,
  /// Scheme the mock server is using
  pub scheme: MockServerScheme,
  /// Port the mock server is running on
  pub port: Option<u16>,
  /// Address the mock server is bound to
  pub address: Option<String>,
  /// List of resources that need to be cleaned up when the mock server completes
  pub resources: Vec<CString>,
  /// Pact that this mock server is based on
  pub pact: Arc<Mutex<dyn Pact + Send + Sync>>,
  /// Receiver of match results
  matches: Arc<Mutex<Vec<MatchResult>>>,
  /// Shutdown signal
  shutdown_tx: RefCell<Option<futures::channel::oneshot::Sender<()>>>,
  /// Mock server config
  pub config: MockServerConfig,
  /// Metrics collected by the mock server
  pub metrics: MockServerMetrics,
  /// Pact spec version to use
  pub spec_version: PactSpecification
}

impl MockServer {
  /// Create a new mock server, consisting of its state (self) and its executable server future.
  pub async fn new(
    id: String,
    pact: Box<dyn Pact + Send + Sync>,
    addr: std::net::SocketAddr,
    config: MockServerConfig
  ) -> Result<(Arc<Mutex<MockServer>>, impl std::future::Future<Output = ()>), String> {
    let (shutdown_tx, shutdown_rx) = futures::channel::oneshot::channel();
    let matches = Arc::new(Mutex::new(vec![]));

    let mock_server = Arc::new(Mutex::new(MockServer {
      id: id.clone(),
      port: None,
      address: None,
      scheme: MockServerScheme::HTTP,
      resources: vec![],
      pact: pact.thread_safe(),
      matches: matches.clone(),
      shutdown_tx: RefCell::new(Some(shutdown_tx)),
      config: config.clone(),
      metrics: MockServerMetrics::default(),
      spec_version: pact_specification(config.pact_specification, pact.specification_version())
    }));

    let (future, socket_addr) = hyper_server::create_and_bind(
      pact.thread_safe(),
      addr,
      async {
        shutdown_rx.await.ok();
      },
      matches,
      mock_server.clone(),
      &id
    )
      .await
      .map_err(|err| format!("Could not start server: {}", err))?;

    {
      let mut ms = mock_server.lock().unwrap();
      ms.deref_mut().port = Some(socket_addr.port());
      ms.deref_mut().address = Some(socket_addr.ip().to_string());

      debug!("Started mock server on {}:{}", socket_addr.ip(), socket_addr.port());
    }

    Ok((mock_server.clone(), future))
  }

  /// Create a new TLS mock server, consisting of its state (self) and its executable server future.
  pub async fn new_tls(
    id: String,
    pact: Box<dyn Pact>,
    addr: std::net::SocketAddr,
    tls: &ServerConfig,
    config: MockServerConfig
  ) -> Result<(Arc<Mutex<MockServer>>, impl std::future::Future<Output = ()>), String> {
    let (shutdown_tx, shutdown_rx) = futures::channel::oneshot::channel();
    let matches = Arc::new(Mutex::new(vec![]));
    let mock_server = Arc::new(Mutex::new(MockServer {
      id: id.clone(),
      port: None,
      address: None,
      scheme: MockServerScheme::HTTPS,
      resources: vec![],
      pact: pact.thread_safe(),
      matches: matches.clone(),
      shutdown_tx: RefCell::new(Some(shutdown_tx)),
      config: config.clone(),
      metrics: MockServerMetrics::default(),
      spec_version: pact_specification(config.pact_specification, pact.specification_version())
    }));

    let (future, socket_addr) = hyper_server::create_and_bind_tls(
      pact.thread_safe(),
      addr,
      async {
        shutdown_rx.await.ok();
      },
      matches,
      tls.clone(),
      mock_server.clone()
    ).await.map_err(|err| format!("Could not start server: {}", err))?;

    {
      let mut ms = mock_server.lock().unwrap();
      ms.deref_mut().port = Some(socket_addr.port());
      ms.deref_mut().address = Some(socket_addr.ip().to_string());

      debug!("Started mock server on {}:{}", socket_addr.ip(), socket_addr.port());
    }

    Ok((mock_server.clone(), future))
  }

  /// Send the shutdown signal to the server
  pub fn shutdown(&mut self) -> Result<(), String> {
    // Need to check if any plugins need to be shutdown
    let pact = self.pact.lock().unwrap();
    for plugin in pact.plugin_data() {
      let dependency = PluginDependency {
        name: plugin.name,
        version: Some(plugin.version),
        dependency_type: PluginDependencyType::Plugin
      };
      drop_plugin_access(&dependency);
    }

    let shutdown_future = &mut *self.shutdown_tx.borrow_mut();
    match shutdown_future.take() {
      Some(sender) => {
        match sender.send(()) {
          Ok(()) => {
            debug!("Mock server {} shutdown - {:?}", self.id, self.metrics);
            Ok(())
          },
          Err(_) => Err("Problem sending shutdown signal to mock server".into())
        }
      },
      _ => Err("Mock server already shut down".into())
    }
  }

    /// Converts this mock server to a `Value` struct
    pub fn to_json(&self) -> serde_json::Value {
      let pact = self.pact.lock().unwrap();
      json!({
        "id" : self.id.clone(),
        "port" : self.port.unwrap_or_default() as u64,
        "address" : self.address.clone().unwrap_or_default(),
        "scheme" : self.scheme.to_string(),
        "provider" : pact.provider().name.clone(),
        "status" : if self.mismatches().is_empty() { "ok" } else { "error" },
        "metrics" : self.metrics
      })
    }

    /// Returns all collected matches
    pub fn matches(&self) -> Vec<MatchResult> {
        self.matches.lock().unwrap().clone()
    }

    /// Returns all the mismatches that have occurred with this mock server
    pub fn mismatches(&self) -> Vec<MatchResult> {
      let matches = self.matches();
      let mismatches = matches.iter()
        .filter(|m| !m.matched() && !m.cors_preflight())
        .map(|m| m.clone());
      let requests: Vec<HttpRequest> = matches.iter().map(|m| {
        match m {
          MatchResult::RequestMatch(request, _) => Some(request),
          MatchResult::RequestMismatch(request, _) => Some(request),
          MatchResult::RequestNotFound(_) => None,
          MatchResult::MissingRequest(_) => None
        }
      }).filter(|o| o.is_some()).map(|o| o.unwrap().clone()).collect();

      let pact = self.pact.lock().unwrap();
      let interactions = pact.interactions();
      let missing = interactions.iter()
        .map(|i| i.as_v4_http().unwrap().request)
        .filter(|req| !requests.contains(req))
        .map(|req| MatchResult::MissingRequest(req.clone()));
      mismatches.chain(missing).collect()
    }

  /// Mock server writes its pact out to the provided directory
  pub fn write_pact(&self, output_path: &Option<String>, overwrite: bool) -> anyhow::Result<()> {
    trace!("write_pact: output_path = {:?}, overwrite = {}", output_path, overwrite);
    let mut pact = self.pact.lock().unwrap();
    pact.add_md_version("mockserver", option_env!("CARGO_PKG_VERSION").unwrap_or("unknown"));
    let pact_file_name = pact.default_file_name();
    let filename = match *output_path {
      Some(ref path) => {
        let mut path = PathBuf::from(path);
        path.push(pact_file_name);
        path
      },
      None => PathBuf::from(pact_file_name)
    };

    info!("Writing pact out to '{}'", filename.display());
    let specification = match self.spec_version {
      PactSpecification::Unknown => PactSpecification::V3,
      _ => self.spec_version
    };
    match write_pact(pact.boxed(), filename.as_path(), specification, overwrite) {
      Ok(_) => Ok(()),
      Err(err) => {
        warn!("Failed to write pact to file - {}", err);
        Err(err)
      }
    }
  }

    /// Returns the URL of the mock server
    pub fn url(&self) -> String {
      let addr = self.address.clone().unwrap_or_else(|| "127.0.0.1".to_string());
      match self.port {
        Some(port) => format!("{}://{}:{}", self.scheme.to_string(),
          if addr == "0.0.0.0" { "127.0.0.1" } else { addr.as_str() }, port),
        None => "error(port is not set)".to_string()
      }
    }
}

fn pact_specification(spec1: PactSpecification, spec2: PactSpecification) -> PactSpecification {
  match spec1 {
    PactSpecification::Unknown => spec2,
    _ => spec1
  }
}

impl Clone for MockServer {
  /// Make a clone all of the MockServer fields.
  /// Note that the clone of the original server cannot be shut down directly.
  fn clone(&self) -> MockServer {
    MockServer {
      id: self.id.clone(),
      port: self.port,
      address: self.address.clone(),
      scheme: self.scheme.clone(),
      resources: vec![],
      pact: self.pact.clone(),
      matches: self.matches.clone(),
      shutdown_tx: RefCell::new(None),
      config: self.config.clone(),
      metrics: self.metrics.clone(),
      spec_version: self.spec_version
    }
  }
}

impl Default for MockServer {
  fn default() -> Self {
    MockServer {
      id: "".to_string(),
      scheme: Default::default(),
      port: None,
      address: None,
      resources: vec![],
      pact: Arc::new(Mutex::new(RequestResponsePact::default())),
      matches: Arc::new(Mutex::new(vec![])),
      shutdown_tx: RefCell::new(None),
      config: Default::default(),
      metrics: Default::default(),
      spec_version: Default::default()
    }
  }
}
