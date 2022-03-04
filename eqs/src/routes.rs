use crate::api_server::WebState;
use crate::query_result_state::QueryResultState;
use crate::route_parsing::*;

use cap_rust_sandbox::ledger::{CapeLedger, CommittedCapeTransition};
use cap_rust_sandbox::model::CapeLedgerState;
use jf_cap::structs::Nullifier;
use net::server::response;
use seahorse::events::LedgerEvent;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use strum::IntoEnumIterator;
use strum_macros::{AsRefStr, EnumIter, EnumString};

/// Index entries for documentation fragments
#[allow(non_camel_case_types)]
#[derive(AsRefStr, Copy, Clone, Debug, EnumIter, EnumString)]
pub enum ApiRouteKey {
    get_cap_state,
    get_all_nullifiers,
    check_nullifier,
    get_events_since,
    get_transaction,
    // get_transaction_by_hash,
}

/// Verify that every variant of enum ApiRouteKey is defined in api.toml
// TODO !corbett Check all the other things that might fail after startup.
pub fn check_api(api: toml::Value) -> bool {
    let mut missing_definition = false;
    for key in ApiRouteKey::iter() {
        let key_str = key.as_ref();
        if api["route"].get(key_str).is_none() {
            println!("Missing API definition for [route.{}]", key_str);
            missing_definition = true;
        }
    }
    if missing_definition {
        panic!("api.toml is inconsistent with enum ApiRouteKey");
    }
    !missing_definition
}

#[allow(dead_code)]
pub fn dummy_url_eval(
    route_pattern: &str,
    bindings: &HashMap<String, RouteBinding>,
) -> Result<tide::Response, tide::Error> {
    let route_str = route_pattern.to_string();
    let title = route_pattern.split_once('/').unwrap_or((&route_str, "")).0;
    Ok(tide::Response::builder(200)
        .body(tide::Body::from_string(format!(
            "<!DOCTYPE html>
<html lang='en'>
  <head>
    <meta charset='utf-8'>
    <title>{}</title>
    <link rel='stylesheet' href='style.css'>
    <script src='script.js'></script>
  </head>
  <body>
    <h1>{}</h1>
    <p>{:?}</p>
  </body>
</html>",
            title, route_str, bindings
        )))
        .content_type(tide::http::mime::HTML)
        .build())
}

////////////////////////////////////////////////////////////////////////////////
// Endpoints
//
// Each endpoint function handles one API endpoint, returning an instance of
// Serialize (or an error). The main entrypoint, dispatch_url, is in charge of
// serializing the endpoint responses according to the requested content type
// and building a Response object.
//

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CapState {
    pub ledger: CapeLedgerState,
    pub num_events: u64,
}

pub async fn get_cap_state(query_result_state: &QueryResultState) -> Result<CapState, tide::Error> {
    Ok(CapState {
        ledger: query_result_state.ledger_state.clone(),
        num_events: query_result_state.events.len() as u64,
    })
}

pub async fn get_all_nullifiers(
    query_result_state: &QueryResultState,
) -> Result<HashSet<Nullifier>, tide::Error> {
    Ok(query_result_state.nullifiers.clone())
}

pub async fn check_nullifier(
    bindings: &HashMap<String, RouteBinding>,
    query_result_state: &QueryResultState,
) -> Result<bool, tide::Error> {
    Ok(query_result_state
        .nullifiers
        .contains(&bindings[":nullifier"].value.to::<Nullifier>()?))
}

pub async fn get_events_since(
    bindings: &HashMap<String, RouteBinding>,
    query_result_state: &QueryResultState,
) -> Result<Vec<LedgerEvent<CapeLedger>>, tide::Error> {
    let first = if let Some(first) = bindings.get(":first") {
        first.value.as_u64()? as usize
    } else {
        0
    };
    let events_len = query_result_state.events.len();
    if first >= events_len {
        return Ok(Vec::new());
    }
    let last = if let Some(max_count) = bindings.get(":max_count") {
        std::cmp::min(first + max_count.value.as_u64()? as usize, events_len)
    } else {
        events_len
    };
    Ok(query_result_state.events[first..last].to_vec())
}

pub async fn get_transaction(
    bindings: &HashMap<String, RouteBinding>,
    query_result_state: &QueryResultState,
) -> Result<Option<CommittedCapeTransition>, tide::Error> {
    Ok(query_result_state
        .transaction_by_id
        .get(&(
            bindings[":block_id"].value.as_u64()?,
            bindings[":txn_id"].value.as_u64()?,
        ))
        .cloned())
}

// pub async fn get_transaction_by_hash(
//     bindings: &HashMap<String, RouteBinding>,
//     query_result_state: &QueryResultState,
// ) -> Result<Option<CommittedCapeTransition>, tide::Error> {
//     if let Some(txn_id) = query_result_state
//         .transaction_id_by_hash
//         .get(&bindings[":hash"].value.to::<Commitment<CapeTransition>>()?)
//     {
//         Ok(query_result_state.transaction_by_id.get(txn_id).cloned())
//     } else {
//         Ok(None)
//     }
// }

pub async fn dispatch_url(
    req: tide::Request<WebState>,
    route_pattern: &str,
    bindings: &HashMap<String, RouteBinding>,
) -> Result<tide::Response, tide::Error> {
    let segments = route_pattern.split_once('/').unwrap_or((route_pattern, ""));
    let key = ApiRouteKey::from_str(segments.0).expect("Unknown route");
    let query_state_guard = req.state().query_result_state.read().await;
    let query_state = &*query_state_guard;
    match key {
        ApiRouteKey::get_cap_state => response(&req, get_cap_state(query_state).await?),
        ApiRouteKey::get_all_nullifiers => response(&req, get_all_nullifiers(query_state).await?),
        ApiRouteKey::check_nullifier => {
            response(&req, check_nullifier(bindings, query_state).await?)
        }
        ApiRouteKey::get_events_since => {
            response(&req, get_events_since(bindings, query_state).await?)
        }
        ApiRouteKey::get_transaction => {
            response(&req, get_transaction(bindings, query_state).await?)
        } // ApiRouteKey::get_transaction_by_hash => {
          //     response(&req, get_transaction_by_hash(bindings, query_state).await?)
          // }
    }
}
