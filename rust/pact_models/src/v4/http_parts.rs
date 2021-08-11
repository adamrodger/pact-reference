//! V4 specification models - HTTP parts for SynchronousHttp

use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::hash::{Hash, Hasher};

use base64::decode;
use bytes::BytesMut;
use log::*;
use serde_json::{json, Value};

use crate::bodies::OptionalBody;
use crate::content_types::{ContentType, detect_content_type_from_bytes};
use crate::generators::{Generators, generators_from_json, generators_to_json};
use crate::json_utils::{headers_from_json, json_to_string};
use crate::matchingrules::{matchers_from_json, matchers_to_json, MatchingRules};
use crate::PactSpecification;
use crate::query_strings::{query_to_json, v3_query_from_json};
use crate::request::Request;
use crate::response::Response;
use crate::v4::calc_content_type;

/// Struct that defines the HTTP request.
#[derive(Debug, Clone, Eq)]
pub struct HttpRequest {
  /// Request method
  pub method: String,
  /// Request path
  pub path: String,
  /// Request query string
  pub query: Option<HashMap<String, Vec<String>>>,
  /// Request headers
  pub headers: Option<HashMap<String, Vec<String>>>,
  /// Request body
  pub body: OptionalBody,
  /// Request matching rules
  pub matching_rules: MatchingRules,
  /// Request generators
  pub generators: Generators
}

impl HttpRequest {
  /// Builds a `HttpRequest` from a JSON `Value` struct.
  pub fn from_json(request_json: &Value) -> anyhow::Result<Self> {
    let method_val = match request_json.get("method") {
      Some(v) => match *v {
        Value::String(ref s) => s.to_uppercase(),
        _ => v.to_string().to_uppercase()
      },
      None => "GET".to_string()
    };
    let path_val = match request_json.get("path") {
      Some(v) => match *v {
        Value::String(ref s) => s.clone(),
        _ => v.to_string()
      },
      None => "/".to_string()
    };
    let query_val = match request_json.get("query") {
      Some(v) => v3_query_from_json(v, &PactSpecification::V4),
      None => None
    };
    let headers = headers_from_json(request_json);
    Ok(HttpRequest {
      method: method_val,
      path: path_val,
      query: query_val,
      headers: headers.clone(),
      body: body_from_json(request_json, "body", &headers),
      matching_rules: matchers_from_json(request_json, &None)?,
      generators: generators_from_json(request_json)?,
    })
  }

  /// Converts this `HttpRequest` to a `Value` struct.
  pub fn to_json(&self) -> Value {
    let mut json = json!({
      "method": Value::String(self.method.to_uppercase()),
      "path": Value::String(self.path.clone())
    });
    {
      let map = json.as_object_mut().unwrap();

      if let Some(ref query) = self.query {
        map.insert("query".to_string(), query_to_json(query.clone(), &PactSpecification::V4));
      }

      if let Some(ref headers) = self.headers {
        map.insert("headers".to_string(), Value::Object(
          headers.iter().map(|(k, v)| (k.clone(), json!(v))).collect()
        ));
      }

      if let Value::Object(body) = self.body.to_v4_json() {
        map.insert("body".to_string(), Value::Object(body));
      }

      if self.matching_rules.is_not_empty() {
        map.insert("matchingRules".to_string(), matchers_to_json(
          &self.matching_rules.clone(), &PactSpecification::V4));
      }

      if self.generators.is_not_empty() {
        map.insert("generators".to_string(), generators_to_json(
          &self.generators.clone(), &PactSpecification::V4));
      }
    }
    json
  }

  /// Convert this request to a V3 request struct
  pub fn as_v3_request(&self) -> Request {
    Request {
      method: self.method.clone(),
      path: self.path.clone(),
      query: self.query.clone(),
      headers: self.headers.clone(),
      body: self.body.clone(),
      matching_rules: self.matching_rules.clone(),
      generators: self.generators.clone()
    }
  }

  /// Determine the content type of the request. Returns the content type of the body, otherwise
  /// if a `Content-Type` header is present, the value of that header will be returned.
  /// Otherwise, the body will be inspected.
  pub fn content_type(&self) -> Option<ContentType> {
    calc_content_type(&self.body, &self.headers)
  }
}

impl PartialEq for HttpRequest {
  fn eq(&self, other: &Self) -> bool {
    self.method == other.method && self.path == other.path && self.query == other.query &&
      self.headers == other.headers && self.body == other.body &&
      self.matching_rules == other.matching_rules && self.generators == other.generators
  }
}

impl Hash for HttpRequest {
  fn hash<H: Hasher>(&self, state: &mut H) {
    self.method.hash(state);
    self.path.hash(state);

    if let Some(ref query) = self.query {
      for (k, v) in query {
        k.hash(state);
        v.hash(state);
      }
    }

    if let Some(ref headers) = self.headers {
      for (k, v) in headers {
        k.hash(state);
        v.hash(state);
      }
    }

    self.body.hash(state);
    self.matching_rules.hash(state);
    self.generators.hash(state);
  }
}

pub fn body_from_json(json: &Value, attr_name: &str, headers: &Option<HashMap<String, Vec<String>>>) -> OptionalBody {
  match json.get(attr_name) {
    Some(body) => match *body {
      Value::Object(ref body_attrs) => {
        match body_attrs.get("content") {
          Some(body_contents) => {
            let content_type = match body_attrs.get("contentType") {
              Some(v) => {
                let content_type_str = json_to_string(v);
                match ContentType::parse(&*content_type_str) {
                  Ok(ct) => Some(ct),
                  Err(err) => {
                    warn!("Failed to parse body content type '{}' - {}", content_type_str, err);
                    None
                  }
                }
              },
              None => {
                warn!("Body has no content type set, will default to any headers or metadata");
                match headers {
                  Some(ref h) => match h.iter().find(|kv| kv.0.to_lowercase() == "content-type") {
                    Some((_, v)) => {
                      match ContentType::parse(v[0].as_str()) {
                        Ok(v) => Some(v),
                        Err(err) => {
                          warn!("Failed to parse body content type '{}' - {}", v[0], err);
                          None
                        }
                      }
                    },
                    None => None
                  },
                  None => None
                }
              }
            };

            let (encoded, encoding) = match body_attrs.get("encoded") {
              Some(v) => match *v {
                Value::String(ref s) => (true, s.clone()),
                Value::Bool(b) => (b, Default::default()),
                _ => (true, v.to_string())
              },
              None => (false, Default::default())
            };

            let body_bytes = if encoded {
              match encoding.as_str() {
                "base64" => {
                  match decode(json_to_string(body_contents)) {
                    Ok(bytes) => bytes,
                    Err(err) => {
                      warn!("Failed to decode base64 encoded body - {}", err);
                      json_to_string(body_contents).into()
                    }
                  }
                },
                "json" => body_contents.to_string().into(),
                _ => {
                  warn!("Unrecognised body encoding scheme '{}', will use the raw body", encoding);
                  json_to_string(body_contents).into()
                }
              }
            } else {
              json_to_string(body_contents).into()
            };

            if body_bytes.is_empty() {
              OptionalBody::Empty
            } else {
              let content_type = content_type.unwrap_or_else(|| {
                detect_content_type_from_bytes(&body_bytes).unwrap_or_default()
              });
              let mut buf = BytesMut::new();
              buf.extend_from_slice(&*body_bytes);
              OptionalBody::Present(buf.freeze(), Some(content_type))
            }
          },
          None => OptionalBody::Missing
        }
      },
      Value::Null => OptionalBody::Null,
      _ => {
        warn!("Body in attribute '{}' from JSON file is not formatted correctly, will load it as plain text", attr_name);
        OptionalBody::Present(body.to_string().into(), None)
      }
    },
    None => OptionalBody::Missing
  }
}

impl Display for HttpRequest {
  fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
    write!(f, "HTTP Request ( method: {}, path: {}, query: {:?}, headers: {:?}, body: {} )",
           self.method, self.path, self.query, self.headers, self.body)
  }
}

impl Default for HttpRequest {
  fn default() -> Self {
    HttpRequest {
      method: "GET".into(),
      path: "/".into(),
      query: None,
      headers: None,
      body: OptionalBody::Missing,
      matching_rules: MatchingRules::default(),
      generators: Generators::default()
    }
  }
}

/// Struct that defines the HTTP response.
#[derive(Debug, Clone, Eq)]
pub struct HttpResponse {
  /// Response status
  pub status: u16,
  /// Response headers
  pub headers: Option<HashMap<String, Vec<String>>>,
  /// Response body
  pub body: OptionalBody,
  /// Response matching rules
  pub matching_rules: MatchingRules,
  /// Response generators
  pub generators: Generators
}

impl Display for HttpResponse {
  fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
    write!(f, "HTTP Response ( status: {}, headers: {:?}, body: {} )", self.status, self.headers,
           self.body)
  }
}

impl Default for HttpResponse {
  fn default() -> Self {
    HttpResponse {
      status: 200,
      headers: None,
      body: OptionalBody::Missing,
      matching_rules: MatchingRules::default(),
      generators: Generators::default()
    }
  }
}

impl PartialEq for HttpResponse {
  fn eq(&self, other: &Self) -> bool {
    self.status == other.status && self.headers == other.headers && self.body == other.body &&
      self.matching_rules == other.matching_rules && self.generators == other.generators
  }
}

impl Hash for HttpResponse {
  fn hash<H: Hasher>(&self, state: &mut H) {
    self.status.hash(state);

    if let Some(ref headers) = self.headers {
      for (k, v) in headers {
        k.hash(state);
        v.hash(state);
      }
    }

    self.body.hash(state);
    self.matching_rules.hash(state);
    self.generators.hash(state);
  }
}

impl HttpResponse {
  /// Build an `HttpResponse` from a JSON `Value` struct.
  pub fn from_json(response: &Value) -> anyhow::Result<Self> {
    let status_val = match response.get("status") {
      Some(v) => v.as_u64().unwrap() as u16,
      None => 200
    };
    let headers = headers_from_json(response);
    Ok(HttpResponse {
      status: status_val,
      headers: headers.clone(),
      body: body_from_json(response, "body", &headers),
      matching_rules: matchers_from_json(response, &None)?,
      generators: generators_from_json(response)?,
    })
  }

  /// Converts this response to a `Value` struct.
  pub fn to_json(&self) -> Value {
    let mut json = json!({
      "status" : self.status
    });
    {
      let map = json.as_object_mut().unwrap();

      if let Some(ref headers) = self.headers {
        map.insert("headers".to_string(), Value::Object(
          headers.iter().map(|(k, v)| (k.clone(), json!(v))).collect()
        ));
      }

      if let Value::Object(body) = self.body.to_v4_json() {
        map.insert("body".to_string(), Value::Object(body));
      }

      if self.matching_rules.is_not_empty() {
        map.insert("matchingRules".to_string(), matchers_to_json(
          &self.matching_rules.clone(), &PactSpecification::V4));
      }

      if self.generators.is_not_empty() {
        map.insert("generators".to_string(), generators_to_json(
          &self.generators.clone(), &PactSpecification::V4));
      }
    }
    json
  }

  /// Converts this response to a v3 response struct
  pub fn as_v3_response(&self) -> Response {
    Response {
      status: self.status,
      headers: self.headers.clone(),
      body: self.body.clone(),
      matching_rules: self.matching_rules.clone(),
      generators: self.generators.clone()
    }
  }

  /// Determine the content type of the response. Returns the content type of the body, otherwise
  /// if a `Content-Type` header is present, the value of that header will be returned.
  /// Otherwise, the body will be inspected.
  pub fn content_type(&self) -> Option<ContentType> {
    calc_content_type(&self.body, &self.headers)
  }
}

#[cfg(test)]
mod tests {
  use std::collections::hash_map::DefaultHasher;
  use std::hash::{Hash, Hasher};

  use expectest::prelude::*;
  use maplit::hashmap;
  use serde_json::json;

  use crate::bodies::OptionalBody;
  use crate::content_types::JSON;
  use crate::json_utils::headers_from_json;
  use crate::v4::http_parts::{body_from_json, HttpRequest, HttpResponse};

  #[test]
  fn synchronous_http_request_from_json_defaults_to_get() {
    let request_json : serde_json::Value = serde_json::from_str(r#"
    {
        "path": "/",
        "query": "",
        "headers": {}
    }
   "#).unwrap();
    let request = HttpRequest::from_json(&request_json);
    expect!(request.unwrap().method).to(be_equal_to("GET"));
  }

  #[test]
  fn synchronous_http_request_from_json_defaults_to_root_for_path() {
    let request_json : serde_json::Value = serde_json::from_str(r#"
      {
          "method": "PUT",
          "query": "",
          "headers": {}
      }
     "#).unwrap();
    let request = HttpRequest::from_json(&request_json);
    assert_eq!(request.unwrap().path, "/".to_string());
  }

  #[test]
  fn synchronous_http_response_from_json_defaults_to_status_200() {
    let response_json : serde_json::Value = serde_json::from_str(r#"
    {
        "headers": {}
    }
   "#).unwrap();
    let response = HttpResponse::from_json(&response_json);
    assert_eq!(response.unwrap().status, 200);
  }

  #[test]
  fn synchronous_http_request_content_type_falls_back_the_content_type_header_and_then_the_contents() {
    let request_json = json!({
      "headers": {},
      "body": {
        "content": "string"
      }
    });
    let request = HttpRequest::from_json(&request_json);
    expect!(request.unwrap().body.content_type().unwrap()).to(be_equal_to("text/plain"));

    let request_json = json!({
      "headers": {
        "Content-Type": ["text/html"]
      },
      "body": {
        "content": "string"
      }
    });
    let request = HttpRequest::from_json(&request_json);
    expect!(request.unwrap().body.content_type().unwrap()).to(be_equal_to("text/html"));

    let request_json = json!({
      "headers": {
        "Content-Type": ["application/json; charset=UTF-8"]
      },
      "body": {
        "content": "string"
      }
    });
    let request = HttpRequest::from_json(&request_json);
    expect!(request.unwrap().body.content_type().unwrap()).to(be_equal_to("application/json;charset=utf-8"));

    let request_json = json!({
      "headers": {
        "CONTENT-TYPE": ["application/json; charset=UTF-8"]
      },
      "body": {
        "content": "string"
      }
    });
      let request = HttpRequest::from_json(&request_json);
      expect!(request.unwrap().body.content_type().unwrap()).to(be_equal_to("application/json;charset=utf-8"));

      let request_json = json!({
      "body": {
        "content": { "json": true }
      }
    });
    let request = HttpRequest::from_json(&request_json);
    expect!(request.unwrap().body.content_type().unwrap()).to(be_equal_to("application/json"));
  }

  #[test]
  fn http_request_to_json_with_defaults() {
    let request = HttpRequest::default();
    expect!(request.to_json().to_string()).to(
      be_equal_to("{\"method\":\"GET\",\"path\":\"/\"}"));
  }

  #[test]
  fn http_request_to_json_converts_methods_to_upper_case() {
    let request = HttpRequest { method: "post".into(), .. HttpRequest::default() };
    expect!(request.to_json().to_string()).to(be_equal_to("{\"method\":\"POST\",\"path\":\"/\"}"));
  }

  #[test]
  fn http_request_to_json_with_a_query() {
    let request = HttpRequest { query: Some(hashmap!{
        "a".to_string() => vec!["1".to_string(), "2".to_string()],
        "b".to_string() => vec!["3".to_string()]
    }), .. HttpRequest::default() };
    expect!(request.to_json().to_string()).to(
      be_equal_to(r#"{"method":"GET","path":"/","query":{"a":["1","2"],"b":["3"]}}"#)
    );
  }

  #[test]
  fn http_request_to_json_with_headers() {
    let request = HttpRequest { headers: Some(hashmap!{
    "HEADERA".to_string() => vec!["VALUEA".to_string()],
    "HEADERB".to_string() => vec!["VALUEB1, VALUEB2".to_string()]
  }), .. HttpRequest::default() };
    expect!(request.to_json().to_string()).to(
      be_equal_to(r#"{"headers":{"HEADERA":["VALUEA"],"HEADERB":["VALUEB1, VALUEB2"]},"method":"GET","path":"/"}"#)
    );
  }

  #[test]
  fn http_request_to_json_with_json_body() {
    let request = HttpRequest { headers: Some(hashmap!{
    "Content-Type".to_string() => vec!["application/json".to_string()]
  }), body: OptionalBody::Present(r#"{"key": "value"}"#.into(), Some("application/json".into())), .. HttpRequest::default() };
    expect!(request.to_json().to_string()).to(
      be_equal_to(r#"{"body":{"content":{"key":"value"},"contentType":"application/json","encoded":false},"headers":{"Content-Type":["application/json"]},"method":"GET","path":"/"}"#)
    );
  }

  #[test]
  fn http_request_to_json_with_non_json_body() {
    let request = HttpRequest { headers: Some(hashmap!{ "Content-Type".to_string() => vec!["text/plain".to_string()] }),
      body: OptionalBody::Present("This is some text".into(), Some("text/plain".into())), .. HttpRequest::default() };
    expect!(request.to_json().to_string()).to(
      be_equal_to(r#"{"body":{"content":"This is some text","contentType":"text/plain","encoded":false},"headers":{"Content-Type":["text/plain"]},"method":"GET","path":"/"}"#)
    );
  }

  #[test]
  fn http_request_to_json_with_empty_body() {
    let request = HttpRequest { body: OptionalBody::Empty, .. HttpRequest::default() };
    expect!(request.to_json().to_string()).to(
      be_equal_to(r#"{"body":{"content":""},"method":"GET","path":"/"}"#)
    );
  }

  #[test]
  fn http_request_to_json_with_null_body() {
    let request = HttpRequest { body: OptionalBody::Null, .. HttpRequest::default() };
    expect!(request.to_json().to_string()).to(
      be_equal_to(r#"{"method":"GET","path":"/"}"#)
    );
  }

  #[test]
  fn http_response_to_json_with_defaults() {
    let response = HttpResponse::default();
    expect!(response.to_json().to_string()).to(be_equal_to("{\"status\":200}"));
  }

  #[test]
  fn http_response_to_json_with_headers() {
    let response = HttpResponse { headers: Some(hashmap!{
      "HEADERA".to_string() => vec!["VALUEA".to_string()],
      "HEADERB".to_string() => vec!["VALUEB1, VALUEB2".to_string()]
  }), .. HttpResponse::default() };
    expect!(response.to_json().to_string()).to(
      be_equal_to(r#"{"headers":{"HEADERA":["VALUEA"],"HEADERB":["VALUEB1, VALUEB2"]},"status":200}"#)
    );
  }

  #[test]
  fn http_response_to_json_with_json_body() {
    let response = HttpResponse { headers: Some(hashmap!{
        "Content-Type".to_string() => vec!["application/json".to_string()]
    }), body: OptionalBody::Present(r#"{"key": "value"}"#.into(), Some("application/json".into())), .. HttpResponse::default() };
    expect!(response.to_json().to_string()).to(
      be_equal_to(r#"{"body":{"content":{"key":"value"},"contentType":"application/json","encoded":false},"headers":{"Content-Type":["application/json"]},"status":200}"#)
    );
  }

  #[test]
  fn http_response_to_json_with_non_json_body() {
    let response = HttpResponse { headers: Some(hashmap!{ "Content-Type".to_string() => vec!["text/plain".to_string()] }),
      body: OptionalBody::Present("This is some text".into(), "text/plain".parse().ok()), .. HttpResponse::default() };
    expect!(response.to_json().to_string()).to(
      be_equal_to(r#"{"body":{"content":"This is some text","contentType":"text/plain","encoded":false},"headers":{"Content-Type":["text/plain"]},"status":200}"#)
    );
  }

  #[test]
  fn http_response_to_json_with_empty_body() {
    let response = HttpResponse { body: OptionalBody::Empty, .. HttpResponse::default() };
    expect!(response.to_json().to_string()).to(
      be_equal_to(r#"{"body":{"content":""},"status":200}"#)
    );
  }

  #[test]
  fn http_response_to_json_with_null_body() {
    let response = HttpResponse { body: OptionalBody::Null, .. HttpResponse::default() };
    expect!(response.to_json().to_string()).to(
      be_equal_to(r#"{"status":200}"#)
    );
  }

  fn hash<T: Hash>(t: &T) -> u64 {
    let mut s = DefaultHasher::new();
    t.hash(&mut s);
    s.finish()
  }

  #[test]
  fn hash_for_http_request() {
    let request1 = HttpRequest::default();
    let request2 = HttpRequest { method: "POST".to_string(), .. HttpRequest::default() };
    let request3 = HttpRequest { headers: Some(hashmap!{
        "H1".to_string() => vec!["A".to_string()]
    }), .. HttpRequest::default() };
    let request4 = HttpRequest { headers: Some(hashmap!{
        "H1".to_string() => vec!["B".to_string()]
    }), .. HttpRequest::default() };
    expect!(hash(&request1)).to(be_equal_to(hash(&request1)));
    expect!(hash(&request3)).to(be_equal_to(hash(&request3)));
    expect!(hash(&request1)).to_not(be_equal_to(hash(&request2)));
    expect!(hash(&request3)).to_not(be_equal_to(hash(&request4)));
  }

  #[test]
  fn hash_for_http_response() {
    let response1 = HttpResponse::default();
    let response2 = HttpResponse { status: 400, .. HttpResponse::default() };
    let response3 = HttpResponse { headers: Some(hashmap!{
        "H1".to_string() => vec!["A".to_string()]
    }), .. HttpResponse::default() };
    let response4 = HttpResponse { headers: Some(hashmap!{
        "H1".to_string() => vec!["B".to_string()]
    }), .. HttpResponse::default() };
    expect!(hash(&response1)).to(be_equal_to(hash(&response1)));
    expect!(hash(&response3)).to(be_equal_to(hash(&response3)));
    expect!(hash(&response1)).to_not(be_equal_to(hash(&response2)));
    expect!(hash(&response3)).to_not(be_equal_to(hash(&response4)));
  }

  #[test]
  fn body_from_json_returns_missing_if_there_is_no_body() {
    let json = json!({});
    let body = body_from_json(&json, "body", &None);
    expect!(body).to(be_equal_to(OptionalBody::Missing));
  }

  #[test]
  fn body_from_json_returns_null_if_the_body_is_null() {
    let json = json!({
      "path": "/",
      "query": "",
      "headers": {},
      "body": null
    });
    let body = body_from_json(&json, "body", &None);
    expect!(body).to(be_equal_to(OptionalBody::Null));
  }

  #[test]
  fn body_from_json_returns_json_string_if_the_body_is_json_but_not_a_string() {
    let json = json!({
      "path": "/",
      "query": "",
      "headers": {},
      "body": {
        "content": {
          "test": true
        }
      }
    });
    let body = body_from_json(&json, "body", &None);
    expect!(body).to(be_equal_to(OptionalBody::Present("{\"test\":true}".into(),
                                                       Some(JSON.clone()))));
  }

  #[test]
  fn body_from_json_returns_empty_if_the_body_is_an_empty_string() {
    let json = json!({
      "path": "/",
      "query": "",
      "headers": {},
      "body": {
        "content": ""
      }
    });
    let body = body_from_json(&json, "body", &None);
    expect!(body).to(be_equal_to(OptionalBody::Empty));
  }

  #[test]
  fn body_from_json_returns_the_body_if_the_body_is_a_string() {
    let json = json!({
      "path": "/",
      "query": "",
      "headers": {},
      "body": {
        "content": "<?xml version=\"1.0\"?> <body></body>"
      }
    });
    let body = body_from_json(&json, "body", &None);
    expect!(body).to(be_equal_to(
      OptionalBody::Present("<?xml version=\"1.0\"?> <body></body>".into(),
                            Some("application/xml".into()))));
  }

  #[test]
  fn body_from_text_plain_type_returns_the_same_formatted_body() {
    let json = json!({
      "path": "/",
      "query": "",
      "headers": {"Content-Type": "text/plain"},
      "body": {
        "content": "\"This is a string\""
      }
    });
    let headers = headers_from_json(&json);
    let body = body_from_json(&json, "body", &headers);
    expect!(body).to(be_equal_to(OptionalBody::Present("\"This is a string\"".into(), Some("text/plain".into()))));
  }

  #[test]
  fn body_from_text_html_type_returns_the_same_formatted_body() {
    let json = json!({
      "path": "/",
      "query": "",
      "headers": {"Content-Type": "text/html"},
      "body": {
        "content": "\"This is a string\""
      }
    });
    let headers = headers_from_json(&json);
    let body = body_from_json(&json, "body", &headers);
    expect!(body).to(be_equal_to(OptionalBody::Present("\"This is a string\"".into(), Some("text/html".into()))));
  }

  #[test]
  fn body_from_json_returns_the_a_json_formatted_body_if_the_body_is_a_string_and_encoding_is_json() {
    let json = json!({
      "body": {
        "content": "This is actually a JSON string",
        "contentType": "application/json",
        "encoded": "json"
      }
    });
    let body = body_from_json(&json, "body", &None);
    expect!(body).to(be_equal_to(OptionalBody::Present("\"This is actually a JSON string\"".into(), Some("application/json".into()))));
  }

  #[test]
  fn body_from_json_returns_the_raw_body_if_there_is_no_encoded_value() {
    let json = json!({
      "path": "/",
      "query": "",
      "headers": {"Content-Type": "application/json"},
      "body": {
        "content": "{\"test\":true}"
      }
    });
    let headers = headers_from_json(&json);
    let body = body_from_json(&json, "body", &headers);
    expect!(body).to(be_equal_to(OptionalBody::Present("{\"test\":true}".into(), Some("application/json".into()))));
  }

}