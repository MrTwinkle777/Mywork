// Copyright © 2021 Translucence Research, Inc. All rights reserved.

use crate::WebState;
use async_std::sync::{Arc, Mutex};
use jf_aap::{MerkleTree, TransactionVerifyingKey};
use std::collections::HashMap;
use std::fmt::Debug;
use std::path::PathBuf;
use std::str::FromStr;
use strum::IntoEnumIterator;
use strum_macros::{AsRefStr, EnumIter, EnumString};
use tagged_base64::TaggedBase64;
use tide::StatusCode;
use tide_websockets::WebSocketConnection;
use zerok_lib::{
    api::server::response,
    cape_ledger::CapeLedger,
    state::{key_set::KeySet, VerifierKeySet, MERKLE_HEIGHT},
    universal_params::UNIVERSAL_PARAM,
    wallet,
    wallet::{
        loader::{Loader, LoaderMetadata},
        testing::mocks::{MockCapeBackend, MockCapeNetwork, MockLedger},
        WalletBackend, WalletError, WalletStorage,
    },
};

pub type Wallet = wallet::Wallet<'static, MockCapeBackend<'static, LoaderMetadata>, CapeLedger>;

#[derive(Clone, Copy, Debug, EnumString)]
pub enum UrlSegmentType {
    Boolean,
    Hexadecimal,
    Integer,
    TaggedBase64,
    Literal,
}

#[allow(dead_code)]
#[derive(Debug)]
pub enum UrlSegmentValue {
    Boolean(bool),
    Hexadecimal(u128),
    Integer(u128),
    Identifier(TaggedBase64),
    Unparsed(String),
    ParseFailed(UrlSegmentType, String),
    Literal(String),
}

use UrlSegmentValue::*;

#[allow(dead_code)]
impl UrlSegmentValue {
    pub fn parse(ptype: UrlSegmentType, value: &str) -> Option<Self> {
        Some(match ptype {
            UrlSegmentType::Boolean => Boolean(value.parse::<bool>().ok()?),
            UrlSegmentType::Hexadecimal => Hexadecimal(u128::from_str_radix(value, 16).ok()?),
            UrlSegmentType::Integer => Integer(value.parse::<u128>().ok()?),
            UrlSegmentType::TaggedBase64 => Identifier(TaggedBase64::parse(value).ok()?),
            UrlSegmentType::Literal => Literal(String::from(value)),
        })
    }

    pub fn as_boolean(&self) -> Result<bool, tide::Error> {
        if let Boolean(b) = self {
            Ok(*b)
        } else {
            Err(tide::Error::from_str(
                StatusCode::BadRequest,
                format!("expected boolean, got {:?}", self),
            ))
        }
    }

    pub fn as_index(&self) -> Result<usize, tide::Error> {
        if let Integer(ix) = self {
            Ok(*ix as usize)
        } else {
            Err(tide::Error::from_str(
                StatusCode::BadRequest,
                format!("expected index, got {:?}", self),
            ))
        }
    }

    pub fn as_identifier(&self) -> Result<TaggedBase64, tide::Error> {
        if let Identifier(i) = self {
            Ok(i.clone())
        } else {
            Err(tide::Error::from_str(
                StatusCode::BadRequest,
                format!("expected tagged base 64, got {:?}", self),
            ))
        }
    }

    pub fn as_path(&self) -> Result<PathBuf, tide::Error> {
        let tb64 = self.as_identifier()?;
        if tb64.tag() == "PATH" {
            Ok(PathBuf::from(std::str::from_utf8(&tb64.value())?))
        } else {
            Err(tide::Error::from_str(
                StatusCode::BadRequest,
                format!("expected tag PATH, got {}", tb64.tag()),
            ))
        }
    }

    pub fn as_string(&self) -> Result<String, tide::Error> {
        match self {
            Self::Literal(s) => Ok(String::from(s)),
            Self::Identifier(tb64) => Ok(String::from(std::str::from_utf8(&tb64.value())?)),
            _ => Err(tide::Error::from_str(
                StatusCode::BadRequest,
                format!("expected string, got {:?}", self),
            )),
        }
    }
}

#[derive(Debug)]
pub struct RouteBinding {
    /// Placeholder from the route pattern, e.g. :id
    pub parameter: String,

    /// Type for parsing
    pub ptype: UrlSegmentType,

    /// Value
    pub value: UrlSegmentValue,
}

/// Index entries for documentation fragments
#[allow(non_camel_case_types)]
#[derive(AsRefStr, Copy, Clone, Debug, EnumIter, EnumString)]
pub enum ApiRouteKey {
    closewallet,
    deposit,
    freeze,
    getaddress,
    getbalance,
    getinfo,
    importkey,
    mint,
    newasset,
    newkey,
    newwallet,
    openwallet,
    send,
    trace,
    transaction,
    unfreeze,
    unwrap,
    wrap,
}

/// Verifiy that every variant of enum ApiRouteKey is defined in api.toml
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
        panic!("api.toml is inconsistent with enum ApiRoutKey");
    }
    !missing_definition
}

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

fn wallet_error(source: WalletError) -> tide::Error {
    tide::Error::from_str(StatusCode::InternalServerError, source.to_string())
}

pub async fn open_wallet(
    mnemonic: String,
    path: Option<PathBuf>,
    existing: bool,
) -> Result<Wallet, tide::Error> {
    let path = match path {
        Some(path) => path,
        None => {
            let home = std::env::var("HOME").map_err(|_| {
                tide::Error::from_str(
                    StatusCode::InternalServerError,
                    "HOME directory is not set. Please set the server's HOME directory, or specify \
                    a different storage location using :path.",
                )
            })?;
            let mut path = PathBuf::from(home);
            path.push(".translucence/wallet");
            path
        }
    };

    let verif_crs = VerifierKeySet {
        mint: TransactionVerifyingKey::Mint(
            jf_aap::proof::mint::preprocess(&*UNIVERSAL_PARAM, MERKLE_HEIGHT)?.1,
        ),
        xfr: KeySet::new(
            vec![TransactionVerifyingKey::Transfer(
                jf_aap::proof::transfer::preprocess(&*UNIVERSAL_PARAM, 3, 3, MERKLE_HEIGHT)?.1,
            )]
            .into_iter(),
        )
        .unwrap(),
        freeze: KeySet::new(
            vec![TransactionVerifyingKey::Freeze(
                jf_aap::proof::freeze::preprocess(&*UNIVERSAL_PARAM, 2, MERKLE_HEIGHT)?.1,
            )]
            .into_iter(),
        )
        .unwrap(),
    };
    //TODO replace this mock backend with a connection to a real backend when available.
    let ledger = Arc::new(Mutex::new(MockLedger::new(MockCapeNetwork::new(
        verif_crs,
        MerkleTree::new(MERKLE_HEIGHT).unwrap(),
        vec![],
    ))));
    let mut loader = Loader::from_mnemonic(mnemonic, true, path);
    let mut backend = MockCapeBackend::new(ledger.clone(), &mut loader)?;

    if backend.storage().await.exists() != existing {
        return Err(tide::Error::from_str(
            StatusCode::BadRequest,
            if existing {
                "cannot open wallet that does not exist"
            } else {
                "cannot create wallet that already exists"
            },
        ));
    }

    Wallet::new(backend).await.map_err(wallet_error)
}

fn require_wallet(wallet: &mut Option<Wallet>) -> Result<&mut Wallet, tide::Error> {
    wallet.as_mut().ok_or_else(|| {
        tide::Error::from_str(
            StatusCode::BadRequest,
            "you most open a wallet to use this endpoint",
        )
    })
}

////////////////////////////////////////////////////////////////////////////////
// Endpoints
//
// Each endpoint function handles one API endpoint, returning an instance of
// Serialize (or an error). The main entrypoint, dispatch_url, is in charge of
// serializing the endpoint responses according to the requested content type
// and building a Response object.
//

pub async fn newwallet(
    bindings: &HashMap<String, RouteBinding>,
    wallet: &mut Option<Wallet>,
) -> Result<(), tide::Error> {
    let path = match bindings.get(":path") {
        Some(binding) => Some(binding.value.as_path()?),
        None => None,
    };
    let mnemonic = bindings[":mnemonic"].value.as_string()?;

    // If we already have a wallet open, close it before opening a new one, otherwise we can end up
    // with two wallets using the same file at the same time.
    *wallet = None;

    *wallet = Some(open_wallet(mnemonic, path, false).await?);
    Ok(())
}

pub async fn openwallet(
    bindings: &HashMap<String, RouteBinding>,
    wallet: &mut Option<Wallet>,
) -> Result<(), tide::Error> {
    let path = match bindings.get(":path") {
        Some(binding) => Some(binding.value.as_path()?),
        None => None,
    };
    let mnemonic = bindings[":mnemonic"].value.as_string()?;

    // If we already have a wallet open, close it before opening a new one, otherwise we can end up
    // with two wallets using the same file at the same time.
    *wallet = None;

    *wallet = Some(open_wallet(mnemonic, path, true).await?);
    Ok(())
}

async fn closewallet(wallet: &mut Option<Wallet>) -> Result<(), tide::Error> {
    require_wallet(wallet)?;
    *wallet = None;
    Ok(())
}

pub async fn dispatch_url(
    req: tide::Request<WebState>,
    route_pattern: &str,
    bindings: &HashMap<String, RouteBinding>,
) -> Result<tide::Response, tide::Error> {
    let first_segment = route_pattern
        .split_once('/')
        .unwrap_or((route_pattern, ""))
        .0;
    let wallet = &mut *req.state().wallet.lock().await;
    let key = ApiRouteKey::from_str(first_segment).expect("Unknown route");
    match key {
        ApiRouteKey::closewallet => response(&req, closewallet(wallet).await?),
        ApiRouteKey::deposit => dummy_url_eval(route_pattern, bindings),
        ApiRouteKey::freeze => dummy_url_eval(route_pattern, bindings),
        ApiRouteKey::getaddress => dummy_url_eval(route_pattern, bindings),
        ApiRouteKey::getbalance => dummy_url_eval(route_pattern, bindings),
        ApiRouteKey::getinfo => dummy_url_eval(route_pattern, bindings),
        ApiRouteKey::importkey => dummy_url_eval(route_pattern, bindings),
        ApiRouteKey::mint => dummy_url_eval(route_pattern, bindings),
        ApiRouteKey::newasset => dummy_url_eval(route_pattern, bindings),
        ApiRouteKey::newkey => dummy_url_eval(route_pattern, bindings),
        ApiRouteKey::newwallet => response(&req, newwallet(bindings, wallet).await?),
        ApiRouteKey::openwallet => response(&req, openwallet(bindings, wallet).await?),
        ApiRouteKey::send => dummy_url_eval(route_pattern, bindings),
        ApiRouteKey::trace => dummy_url_eval(route_pattern, bindings),
        ApiRouteKey::transaction => dummy_url_eval(route_pattern, bindings),
        ApiRouteKey::unfreeze => dummy_url_eval(route_pattern, bindings),
        ApiRouteKey::unwrap => dummy_url_eval(route_pattern, bindings),
        ApiRouteKey::wrap => dummy_url_eval(route_pattern, bindings),
    }
}

pub async fn dispatch_web_socket(
    _req: tide::Request<WebState>,
    _conn: WebSocketConnection,
    route_pattern: &str,
    _bindings: &HashMap<String, RouteBinding>,
) -> Result<(), tide::Error> {
    let first_segment = route_pattern
        .split_once('/')
        .unwrap_or((route_pattern, ""))
        .0;
    let key = ApiRouteKey::from_str(first_segment).expect("Unknown route");
    match key {
        // ApiRouteKey::subscribe => subscribe(req, conn, bindings).await,
        _ => Err(tide::Error::from_str(
            StatusCode::InternalServerError,
            "server called dispatch_web_socket with an unsupported route",
        )),
    }
}
