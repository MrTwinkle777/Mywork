// Copyright (c) 2022 Espresso Systems (espressosys.com)
// This file is part of the Configurable Asset Privacy for Ethereum (CAPE) library.

// This program is free software: you can redistribute it and/or modify it under the terms of the GNU General Public License as published by the Free Software Foundation, either version 3 of the License, or (at your option) any later version.
// This program is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the GNU General Public License for more details.
// You should have received a copy of the GNU General Public License along with this program. If not, see <https://www.gnu.org/licenses/>.

//! Web server endpoint handlers.

use crate::{
    mocks::{MockCapeBackend, MockCapeNetwork},
    ui::*,
    wallet::{CapeWalletError, CapeWalletExt},
    web::WebState,
};
use async_std::fs::{create_dir_all, File};
use async_std::sync::{Arc, Mutex};
use cap_rust_sandbox::{
    ledger::CapeLedger,
    model::{Erc20Code, EthereumAddr},
    universal_param::UNIVERSAL_PARAM,
};
use futures::{prelude::*, stream::iter};
use jf_cap::{
    keys::{AuditorPubKey, FreezerPubKey, UserKeyPair, UserPubKey},
    structs::{
        AssetCode, AssetDefinition, AssetPolicy, FreezeFlag, ReceiverMemo, RecordCommitment,
        RecordOpening,
    },
    MerkleTree, TransactionVerifyingKey,
};
use key_set::{KeySet, VerifierKeySet};
use net::{server::response, TaggedBlob, UserAddress};
use rand_chacha::ChaChaRng;
use reef::traits::Ledger;
use seahorse::{
    events::{EventIndex, EventSource},
    hd::KeyTree,
    loader::{Loader, LoaderMetadata},
    testing::MockLedger,
    txn_builder::{RecordInfo, TransactionReceipt},
    AssetInfo, WalletBackend, WalletStorage,
};
use serde::{Deserialize, Serialize};
use snafu::Snafu;
use std::collections::HashMap;
use std::fmt::Debug;
use std::path::Path;
use std::path::PathBuf;
use std::str::FromStr;
use strum::IntoEnumIterator;
use strum_macros::{AsRefStr, EnumIter, EnumString};
use tagged_base64::TaggedBase64;
use tide::StatusCode;

#[derive(Debug, Snafu, Serialize, Deserialize)]
#[snafu(module(error))]
pub enum CapeAPIError {
    #[snafu(display("error accessing wallet: {}", msg))]
    Wallet { msg: String },

    #[snafu(display("failed to open wallet: {}", msg))]
    OpenWallet { msg: String },

    #[snafu(display("you must open a wallet to use this enpdoint"))]
    MissingWallet,

    #[snafu(display("invalid parameter: expected {}, got {}", expected, actual))]
    Param { expected: String, actual: String },

    #[snafu(display("invalid TaggedBase64 tag: expected {}, got {}", expected, actual))]
    Tag { expected: String, actual: String },

    #[snafu(display("failed to deserialize request parameter: {}", msg))]
    Deserialize { msg: String },

    #[snafu(display("internal server error: {}", msg))]
    Internal { msg: String },
}

impl net::Error for CapeAPIError {
    fn catch_all(msg: String) -> Self {
        Self::Internal { msg }
    }
    fn status(&self) -> StatusCode {
        match self {
            Self::Param { .. }
            | Self::Tag { .. }
            | Self::Deserialize { .. }
            | Self::OpenWallet { .. }
            | Self::MissingWallet => StatusCode::BadRequest,
            Self::Wallet { .. } | Self::Internal { .. } => StatusCode::InternalServerError,
        }
    }
}

pub fn server_error<E: Into<CapeAPIError>>(err: E) -> tide::Error {
    net::server_error(err)
}

pub type Wallet = seahorse::Wallet<'static, MockCapeBackend<'static, LoaderMetadata>, CapeLedger>;

#[derive(Clone, Copy, Debug, EnumString)]
pub enum UrlSegmentType {
    Boolean,
    Hexadecimal,
    Integer,
    TaggedBase64,
    Base64,
    Literal,
}

#[allow(dead_code)]
#[derive(Clone, Debug, strum_macros::Display)]
pub enum UrlSegmentValue {
    Boolean(bool),
    Hexadecimal(u128),
    Integer(u128),
    Identifier(TaggedBase64),
    Base64(Vec<u8>),
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
            UrlSegmentType::Base64 => {
                Base64(base64::decode_config(value, base64::URL_SAFE_NO_PAD).ok()?)
            }
            UrlSegmentType::Literal => Literal(String::from(value)),
        })
    }

    pub fn as_boolean(&self) -> Result<bool, tide::Error> {
        if let Boolean(b) = self {
            Ok(*b)
        } else {
            Err(server_error(CapeAPIError::Param {
                expected: String::from("Boolean"),
                actual: self.to_string(),
            }))
        }
    }

    pub fn as_index(&self) -> Result<usize, tide::Error> {
        if let Integer(ix) = self {
            Ok(*ix as usize)
        } else {
            Err(server_error(CapeAPIError::Param {
                expected: String::from("Index"),
                actual: self.to_string(),
            }))
        }
    }

    pub fn as_u64(&self) -> Result<u64, tide::Error> {
        if let Integer(i) = self {
            Ok(*i as u64)
        } else {
            Err(server_error(CapeAPIError::Param {
                expected: String::from("Integer"),
                actual: self.to_string(),
            }))
        }
    }

    pub fn as_usize(&self) -> Result<usize, tide::Error> {
        Ok(self.as_u64()? as usize)
    }

    pub fn as_identifier(&self) -> Result<TaggedBase64, tide::Error> {
        if let Identifier(i) = self {
            Ok(i.clone())
        } else {
            Err(server_error(CapeAPIError::Param {
                expected: String::from("TaggedBase64"),
                actual: self.to_string(),
            }))
        }
    }

    pub fn as_base64(&self) -> Result<Vec<u8>, tide::Error> {
        if let Base64(i) = self {
            Ok(i.clone())
        } else {
            Err(server_error(CapeAPIError::Param {
                expected: String::from("Base64"),
                actual: self.to_string(),
            }))
        }
    }

    pub fn as_path(&self) -> Result<PathBuf, tide::Error> {
        Ok(PathBuf::from(std::str::from_utf8(&self.as_base64()?)?))
    }

    pub fn as_string(&self) -> Result<String, tide::Error> {
        match self {
            Self::Literal(s) => Ok(String::from(s)),
            Self::Identifier(tb64) => Ok(String::from(std::str::from_utf8(&tb64.value())?)),
            _ => Err(server_error(CapeAPIError::Param {
                expected: String::from("String"),
                actual: self.to_string(),
            })),
        }
    }

    pub fn to<T: TaggedBlob>(&self) -> Result<T, tide::Error> {
        T::from_tagged_blob(&self.as_identifier()?).map_err(|err| {
            server_error(CapeAPIError::Deserialize {
                msg: err.to_string(),
            })
        })
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
    freeze,
    getaddress,
    getaccount,
    getbalance,
    getinfo,
    getmnemonic,
    importkey,
    mint,
    newasset,
    newkey,
    newwallet,
    openwallet,
    recoverkey,
    send,
    transaction,
    unfreeze,
    unwrap,
    view,
    wrap,
    getrecords,
    lastusedkeystore,
}

/// Verifiy that every variant of enum ApiRouteKey is defined in api.toml
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

pub fn wallet_error(source: CapeWalletError) -> tide::Error {
    server_error(CapeAPIError::Wallet {
        msg: source.to_string(),
    })
}

pub fn get_home_path() -> Result<PathBuf, tide::Error> {
    let home = std::env::var("HOME").map_err(|_| {
        server_error(CapeAPIError::Internal {
            msg: String::from("HOME directory is not set. Please set the server's HOME directory."),
        })
    })?;
    Ok(PathBuf::from(home))
}

pub async fn default_storage_path() -> Result<PathBuf, tide::Error> {
    let mut storage_path = get_home_path().unwrap();
    storage_path.push(".espresso/cape");
    create_dir_all(&storage_path).await?;
    Ok(storage_path)
}

pub async fn get_storage_path(path_arg: &Option<PathBuf>) -> Result<PathBuf, tide::Error> {
    let mut path = if path_arg.is_some() {
        path_arg.as_ref().unwrap().clone()
    } else {
        default_storage_path().await?
    };
    path.push("last_wallet_path");
    Ok(path)
}

pub async fn write_path(
    wallet_path: &Path,
    storage_path: &Option<PathBuf>,
) -> Result<(), tide::Error> {
    let path = get_storage_path(storage_path).await?;
    let mut file = File::create(path).await?;
    Ok(file
        .write_all(&bincode::serialize(&wallet_path).unwrap())
        .await?)
}
pub async fn read_last_path(path_arg: &Option<PathBuf>) -> Result<Option<PathBuf>, tide::Error> {
    let path = get_storage_path(path_arg).await?;
    let file_result = File::open(&path).await;
    if file_result.is_err()
        && file_result.as_ref().err().unwrap().kind() == std::io::ErrorKind::NotFound
    {
        return Ok(None);
    }
    let mut file = file_result?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).await?;
    Ok(Some(bincode::deserialize(&bytes)?))
}
// Create a wallet (if !existing) or open an existing one.
pub async fn init_wallet(
    rng: &mut ChaChaRng,
    faucet_pub_key: UserPubKey,
    mnemonic: Option<String>,
    password: String,
    path: Option<PathBuf>,
    existing: bool,
    storage_path: &Option<PathBuf>,
) -> Result<Wallet, tide::Error> {
    let path = match path {
        Some(path) => path,
        None => {
            let mut path = get_home_path().unwrap();
            path.push(".espresso/cape/wallet");
            path
        }
    };

    // Store the path so we can have a getlastkeystore endpoint
    write_path(&path, storage_path).await?;

    let verif_crs = VerifierKeySet {
        mint: TransactionVerifyingKey::Mint(
            jf_cap::proof::mint::preprocess(&*UNIVERSAL_PARAM, CapeLedger::merkle_height())?.1,
        ),
        xfr: KeySet::new(
            vec![
                TransactionVerifyingKey::Transfer(
                    jf_cap::proof::transfer::preprocess(
                        &*UNIVERSAL_PARAM,
                        2,
                        2,
                        CapeLedger::merkle_height(),
                    )?
                    .1,
                ),
                TransactionVerifyingKey::Transfer(
                    jf_cap::proof::transfer::preprocess(
                        &*UNIVERSAL_PARAM,
                        3,
                        3,
                        CapeLedger::merkle_height(),
                    )?
                    .1,
                ),
            ]
            .into_iter(),
        )
        .unwrap(),
        freeze: KeySet::new(
            vec![TransactionVerifyingKey::Freeze(
                jf_cap::proof::freeze::preprocess(
                    &*UNIVERSAL_PARAM,
                    2,
                    CapeLedger::merkle_height(),
                )?
                .1,
            )]
            .into_iter(),
        )
        .unwrap(),
    };

    // Set up a faucet record.
    let mut records = MerkleTree::new(CapeLedger::merkle_height()).unwrap();
    let faucet_ro = RecordOpening::new(
        rng,
        1000,
        AssetDefinition::native(),
        faucet_pub_key,
        FreezeFlag::Unfrozen,
    );
    records.push(RecordCommitment::from(&faucet_ro).to_field_element());
    let faucet_memo = ReceiverMemo::from_ro(rng, &faucet_ro, &[]).unwrap();

    let mut ledger = MockLedger::new(MockCapeNetwork::new(
        verif_crs,
        records,
        vec![(faucet_memo, 0)],
    ));
    ledger.set_block_size(1).unwrap();

    let mut loader = Loader::from_literal(mnemonic.map(|s| s.replace('-', " ")), password, path);
    let mut backend = MockCapeBackend::new(Arc::new(Mutex::new(ledger)), &mut loader)?;

    if backend.storage().await.exists() != existing {
        return Err(server_error(CapeAPIError::OpenWallet {
            msg: String::from(if existing {
                "cannot open wallet that does not exist"
            } else {
                "cannot create wallet that already exists"
            }),
        }));
    }

    Wallet::new(backend).await.map_err(wallet_error)
}

async fn known_assets(wallet: &Wallet) -> HashMap<AssetCode, AssetInfo> {
    wallet
        .assets()
        .await
        .into_iter()
        .map(|asset| (asset.definition.code, asset))
        .collect()
}

pub fn require_wallet(wallet: &mut Option<Wallet>) -> Result<&mut Wallet, tide::Error> {
    wallet
        .as_mut()
        .ok_or_else(|| server_error(CapeAPIError::MissingWallet))
}

////////////////////////////////////////////////////////////////////////////////
// Endpoints
//
// Each endpoint function handles one API endpoint, returning an instance of
// Serialize (or an error). The main entrypoint, dispatch_url, is in charge of
// serializing the endpoint responses according to the requested content type
// and building a Response object.
//

pub async fn getmnemonic(rng: &mut ChaChaRng) -> Result<String, tide::Error> {
    Ok(KeyTree::random(rng).1.to_string().replace(' ', "-"))
}

pub async fn newwallet(
    bindings: &HashMap<String, RouteBinding>,
    rng: &mut ChaChaRng,
    faucet_key_pair: &UserKeyPair,
    wallet: &mut Option<Wallet>,
    storage_path: &Option<PathBuf>,
) -> Result<(), tide::Error> {
    let path = match bindings.get(":path") {
        Some(binding) => Some(binding.value.as_path()?),
        None => None,
    };
    let mnemonic = bindings[":mnemonic"].value.as_string()?;
    let password = bindings[":password"].value.as_string()?;

    // If we already have a wallet open, close it before opening a new one, otherwise we can end up
    // with two wallets using the same file at the same time.
    *wallet = None;

    *wallet = Some(
        init_wallet(
            rng,
            faucet_key_pair.pub_key(),
            Some(mnemonic),
            password,
            path,
            false,
            storage_path,
        )
        .await?,
    );
    Ok(())
}

pub async fn openwallet(
    bindings: &HashMap<String, RouteBinding>,
    rng: &mut ChaChaRng,
    faucet_key_pair: &UserKeyPair,
    wallet: &mut Option<Wallet>,
    storage_path: &Option<PathBuf>,
) -> Result<(), tide::Error> {
    let path = match bindings.get(":path") {
        Some(binding) => Some(binding.value.as_path()?),
        None => None,
    };
    let password = bindings[":password"].value.as_string()?;

    // If we already have a wallet open, close it before opening a new one, otherwise we can end up
    // with two wallets using the same file at the same time.
    *wallet = None;

    *wallet = Some(
        init_wallet(
            rng,
            faucet_key_pair.pub_key(),
            None,
            password,
            path,
            true,
            storage_path,
        )
        .await?,
    );
    Ok(())
}

async fn closewallet(wallet: &mut Option<Wallet>) -> Result<(), tide::Error> {
    require_wallet(wallet)?;
    *wallet = None;
    Ok(())
}

async fn getinfo(wallet: &mut Option<Wallet>) -> Result<WalletSummary, tide::Error> {
    let wallet = require_wallet(wallet)?;
    Ok(WalletSummary {
        addresses: wallet
            .pub_keys()
            .await
            .into_iter()
            .map(|pub_key| pub_key.address().into())
            .collect(),
        sending_keys: wallet.pub_keys().await,
        viewing_keys: wallet.auditor_pub_keys().await,
        freezing_keys: wallet.freezer_pub_keys().await,
        assets: known_assets(wallet).await.into_values().collect(),
    })
}

async fn getaddress(wallet: &mut Option<Wallet>) -> Result<Vec<UserAddress>, tide::Error> {
    let wallet = require_wallet(wallet)?;
    Ok(wallet
        .pub_keys()
        .await
        .into_iter()
        .map(|pub_key| pub_key.address().into())
        .collect())
}

// Get all balances for the current wallet, all the balances for a given address, or the balance for
// a given address and asset type.
//
// Returns:
//  * BalanceInfo::Balance, if address and asset code both given
//  * BalanceInfo::AccountBalances, if address given
//  * BalanceInfo::AllBalances, if neither given
async fn getbalance(
    bindings: &HashMap<String, RouteBinding>,
    wallet: &mut Option<Wallet>,
) -> Result<BalanceInfo, tide::Error> {
    let wallet = &require_wallet(wallet)?;

    // The request dispatcher should fail if the URL pattern does not match one of the patterns
    // defined for this route in api.toml, so the only routes we have to handle are:
    //  * getbalance/all
    //  * getbalance/address/:address
    //  * getbalance/address/:address/asset/:asset
    // Therefore, we can determine which form we are handling just by checking for the presence of
    // :address and :asset.
    let address = match bindings.get(":address") {
        Some(address) => Some(address.value.to::<UserAddress>()?),
        None => None,
    };
    let asset = match bindings.get(":asset") {
        Some(asset) => Some(asset.value.to::<AssetCode>()?),
        None => None,
    };

    let one_balance = |address: UserAddress, asset| async move {
        wallet.balance_breakdown(&address.into(), &asset).await
    };
    let account_balances = |address: UserAddress| async move {
        iter(known_assets(wallet).await.into_keys())
            .then(|asset| {
                let address = address.clone();
                async move { (asset, one_balance(address, asset).await) }
            })
            .collect()
            .await
    };
    let all_balances = || async {
        iter(wallet.pub_keys().await)
            .then(|key| async move {
                let address = UserAddress::from(key.address());
                (address.clone(), account_balances(address).await)
            })
            .collect()
            .await
    };

    match (address, asset) {
        (Some(address), Some(asset)) => Ok(BalanceInfo::Balance(one_balance(address, asset).await)),
        (Some(address), None) => Ok(BalanceInfo::AccountBalances(
            account_balances(address).await,
        )),
        (None, None) => Ok(BalanceInfo::AllBalances(all_balances().await)),
        (None, Some(_)) => {
            // There is no endpoint that includes asset but not address, so the request parsing code
            // should not allow us to reach here.
            unreachable!()
        }
    }
}

async fn newkey(key_type: &str, wallet: &mut Option<Wallet>) -> Result<PubKey, tide::Error> {
    let wallet = require_wallet(wallet)?;

    match key_type {
        "send" | "sending" => Ok(PubKey::Sending(wallet.generate_user_key(None).await?)),
        "view" | "viewing" => Ok(PubKey::Viewing(wallet.generate_audit_key().await?)),
        "freeze" | "freezing" => Ok(PubKey::Freezing(wallet.generate_freeze_key().await?)),
        _ => Err(server_error(CapeAPIError::Param {
            expected: String::from("key type (sending, viewing or freezing)"),
            actual: String::from(key_type),
        })),
    }
}

async fn newasset(
    bindings: &HashMap<String, RouteBinding>,
    wallet: &mut Option<Wallet>,
) -> Result<AssetDefinition, tide::Error> {
    let wallet = require_wallet(wallet)?;

    // Construct the asset policy.
    let mut policy = AssetPolicy::default();
    if let Some(freezing_key) = bindings.get(":freezing_key") {
        policy = policy.set_freezer_pub_key(freezing_key.value.to::<FreezerPubKey>()?)
    };
    if let Some(viewing_key) = bindings.get(":viewing_key") {
        // Always reveal blinding factor if a viewing key is given.
        policy = policy
            .set_auditor_pub_key(viewing_key.value.to::<AuditorPubKey>()?)
            .reveal_blinding_factor()?;

        // Only if a viewing key is given, can amount and user address be revealed and viewing
        // threshold be specified.
        if let Some(view_flag) = bindings.get(":view_amount") {
            if view_flag.value.as_boolean()? {
                policy = policy.reveal_amount()?;
            }
        }
        if let Some(view_flag) = bindings.get(":view_address") {
            if view_flag.value.as_boolean()? {
                policy = policy.reveal_user_address()?;
            }
        }
        if let Some(threshold) = bindings.get(":viewing_threshold") {
            policy = policy.set_reveal_threshold(threshold.value.as_u64()?);
        };
    };

    // If an ERC20 code is given, sponsor the asset. Otherwise, define an asset.
    match bindings.get(":erc20") {
        Some(erc20_code) => {
            let erc20_code = erc20_code.value.to::<Erc20Code>()?;
            let sponsor_address = bindings
                .get(":sponsor")
                .unwrap()
                .value
                .to::<EthereumAddr>()?;
            Ok(wallet.sponsor(erc20_code, sponsor_address, policy).await?)
        }
        None => {
            let description = match bindings.get(":description") {
                Some(description) => description.value.as_base64()?,
                _ => Vec::new(),
            };
            Ok(wallet.define_asset(&description, policy).await?)
        }
    }
}

async fn wrap(
    bindings: &HashMap<String, RouteBinding>,
    wallet: &mut Option<Wallet>,
) -> Result<(), tide::Error> {
    let wallet = require_wallet(wallet)?;

    let destination = bindings
        .get(":destination")
        .unwrap()
        .value
        .to::<UserAddress>()?;
    let eth_address = bindings
        .get(":eth_address")
        .unwrap()
        .value
        .to::<EthereumAddr>()?;
    let asset = bindings
        .get(":asset")
        .unwrap()
        .value
        .to::<AssetDefinition>()?;
    let amount = bindings.get(":amount").unwrap().value.as_u64()?;

    Ok(wallet
        .wrap(eth_address, asset, destination.into(), amount)
        .await?)
}

async fn mint(
    bindings: &HashMap<String, RouteBinding>,
    wallet: &mut Option<Wallet>,
) -> Result<TransactionReceipt<CapeLedger>, tide::Error> {
    let wallet = require_wallet(wallet)?;

    let asset = bindings.get(":asset").unwrap().value.to::<AssetCode>()?;
    let amount = bindings.get(":amount").unwrap().value.as_u64()?;
    let fee = bindings.get(":fee").unwrap().value.as_u64()?;
    let minter = bindings
        .get(":minter")
        .unwrap()
        .value
        .to::<UserAddress>()?
        .0;
    let recipient = bindings
        .get(":recipient")
        .unwrap()
        .value
        .to::<UserAddress>()?
        .0;

    Ok(wallet.mint(&minter, fee, &asset, amount, recipient).await?)
}

async fn unwrap(
    bindings: &HashMap<String, RouteBinding>,
    wallet: &mut Option<Wallet>,
) -> Result<TransactionReceipt<CapeLedger>, tide::Error> {
    let wallet = require_wallet(wallet)?;

    let source = bindings.get(":source").unwrap().value.to::<UserAddress>()?;
    let eth_address = bindings
        .get(":eth_address")
        .unwrap()
        .value
        .to::<EthereumAddr>()?;
    let asset = bindings.get(":asset").unwrap().value.to::<AssetCode>()?;
    let amount = bindings.get(":amount").unwrap().value.as_u64()?;
    let fee = bindings.get(":fee").unwrap().value.as_u64()?;

    Ok(wallet
        .burn(&source.into(), eth_address, &asset, amount, fee)
        .await?)
}

async fn recoverkey(
    key_type: &str,
    bindings: &HashMap<String, RouteBinding>,
    wallet: &mut Option<Wallet>,
) -> Result<PubKey, tide::Error> {
    let wallet = require_wallet(wallet)?;

    match key_type {
        "send" | "sending" => {
            let scan_from = match bindings.get(":scan_from") {
                Some(param) => param.value.as_usize()?,
                None => 0,
            };
            Ok(PubKey::Sending(
                wallet
                    .generate_user_key(Some(EventIndex::from_source(
                        EventSource::QueryService,
                        scan_from,
                    )))
                    .await?,
            ))
        }
        "view" | "viewing" => Ok(PubKey::Viewing(wallet.generate_audit_key().await?)),
        "freeze" | "freezing" => Ok(PubKey::Freezing(wallet.generate_freeze_key().await?)),
        _ => Err(server_error(CapeAPIError::Param {
            expected: String::from("key type (sending, viewing or freezing)"),
            actual: String::from(key_type),
        })),
    }
}

pub async fn send(
    bindings: &HashMap<String, RouteBinding>,
    wallet: &mut Option<Wallet>,
) -> Result<TransactionReceipt<CapeLedger>, tide::Error> {
    let wallet = require_wallet(wallet)?;

    let src = bindings.get(":sender").unwrap().value.to::<UserAddress>()?;
    let dst = bindings
        .get(":recipient")
        .unwrap()
        .value
        .to::<UserAddress>()?;
    let asset = bindings.get(":asset").unwrap().value.to::<AssetCode>()?;
    let amount = bindings.get(":amount").unwrap().value.as_u64()?;
    let fee = bindings.get(":fee").unwrap().value.as_u64()?;

    wallet
        .transfer(Some(&src.into()), &asset, &[(dst.into(), amount)], fee)
        .await
        .map_err(wallet_error)
}

pub async fn get_records(wallet: &mut Option<Wallet>) -> Result<Vec<RecordInfo>, tide::Error> {
    let wallet = require_wallet(wallet)?;
    Ok(wallet.records().await.collect::<Vec<_>>())
}

pub async fn get_last_keystore(path: &Option<PathBuf>) -> Result<Option<PathBuf>, tide::Error> {
    Ok(read_last_path(path).await?)
}

// Get the set of assets associated with the given codes.
//
// The caller must ensure that each asset code is known to `wallet`.
async fn get_assets(wallet: &Wallet, codes: &[AssetCode]) -> HashMap<AssetCode, AssetInfo> {
    iter(codes)
        .then(|code| async move { (*code, wallet.asset(*code).await.unwrap()) })
        .collect()
        .await
}

async fn get_sending_account(wallet: &Wallet, address: UserAddress) -> Account {
    let (records, asset_codes): (Vec<_>, Vec<_>) = wallet
        .records()
        .await
        .filter_map(|record| {
            if record.ro.pub_key.address() == address.0 {
                Some((record.clone().into(), record.ro.asset_def.code))
            } else {
                None
            }
        })
        .unzip();

    Account {
        records,
        assets: get_assets(wallet, &asset_codes).await,
    }
}

async fn get_viewing_account(wallet: &Wallet, address: AuditorPubKey) -> Account {
    let (records, asset_codes): (Vec<_>, Vec<_>) = wallet
        .records()
        .await
        .filter_map(|record| {
            if record.ro.asset_def.policy_ref().auditor_pub_key() == &address {
                Some((record.clone().into(), record.ro.asset_def.code))
            } else {
                None
            }
        })
        .unzip();
    let mut assets = get_assets(wallet, &asset_codes).await;

    // Make sure assets contains _all_ asset types that are viewable, not just the ones for which we
    // currently have records.
    for asset in wallet.assets().await {
        if asset.definition.policy_ref().auditor_pub_key() == &address {
            assets.insert(asset.definition.code, asset);
        }
    }

    Account { records, assets }
}

async fn get_freezing_account(wallet: &Wallet, address: FreezerPubKey) -> Account {
    let (records, asset_codes): (Vec<_>, Vec<_>) = wallet
        .records()
        .await
        .filter_map(|record| {
            if record.ro.asset_def.policy_ref().freezer_pub_key() == &address {
                Some((record.clone().into(), record.ro.asset_def.code))
            } else {
                None
            }
        })
        .unzip();
    let mut assets = get_assets(wallet, &asset_codes).await;

    // Make sure assets contains _all_ asset types that are freezable??
    for asset in wallet.assets().await {
        if asset.definition.policy_ref().freezer_pub_key() == &address {
            assets.insert(asset.definition.code, asset);
        }
    }

    Account { records, assets }
}

async fn getaccount(
    bindings: &HashMap<String, RouteBinding>,
    wallet: &mut Option<Wallet>,
) -> Result<Account, tide::Error> {
    let wallet = require_wallet(wallet)?;
    let address = bindings[":address"].value.clone();
    match address.as_identifier()?.tag().as_str() {
        "ADDR" => Ok(get_sending_account(wallet, address.to()?).await),
        "USERPUBKEY" => {
            Ok(get_sending_account(wallet, address.to::<UserPubKey>()?.address().into()).await)
        }
        "AUDPUBKEY" => Ok(get_viewing_account(wallet, address.to()?).await),
        "FREEZEPUBKEY" => Ok(get_freezing_account(wallet, address.to()?).await),
        tag => Err(server_error(CapeAPIError::Tag {
            expected: String::from("ADDR | USERPUBKEY | AUDPUBKEY | FREEZEPUBKEY"),
            actual: String::from(tag),
        })),
    }
}

pub async fn dispatch_url(
    req: tide::Request<WebState>,
    route_pattern: &str,
    bindings: &HashMap<String, RouteBinding>,
) -> Result<tide::Response, tide::Error> {
    let segments = route_pattern.split_once('/').unwrap_or((route_pattern, ""));
    let state = req.state();
    let rng = &mut *state.rng.lock().await;
    let faucet_key_pair = &state.faucet_key_pair;
    let wallet = &mut *state.wallet.lock().await;
    let path_storage = &state.path_storage;
    let key = ApiRouteKey::from_str(segments.0).expect("Unknown route");
    match key {
        ApiRouteKey::closewallet => response(&req, closewallet(wallet).await?),
        ApiRouteKey::freeze => dummy_url_eval(route_pattern, bindings),
        ApiRouteKey::getaddress => response(&req, getaddress(wallet).await?),
        ApiRouteKey::getaccount => response(&req, getaccount(bindings, wallet).await?),
        ApiRouteKey::getbalance => response(&req, getbalance(bindings, wallet).await?),
        ApiRouteKey::getinfo => response(&req, getinfo(wallet).await?),
        ApiRouteKey::getmnemonic => response(&req, getmnemonic(rng).await?),
        ApiRouteKey::importkey => dummy_url_eval(route_pattern, bindings),
        ApiRouteKey::mint => response(&req, mint(bindings, wallet).await?),
        ApiRouteKey::newasset => response(&req, newasset(bindings, wallet).await?),
        ApiRouteKey::newkey => response(&req, newkey(segments.1, wallet).await?),
        ApiRouteKey::newwallet => response(
            &req,
            newwallet(bindings, rng, faucet_key_pair, wallet, path_storage).await?,
        ),
        ApiRouteKey::openwallet => response(
            &req,
            openwallet(bindings, rng, faucet_key_pair, wallet, path_storage).await?,
        ),
        ApiRouteKey::recoverkey => response(&req, recoverkey(segments.1, bindings, wallet).await?),
        ApiRouteKey::send => response(&req, send(bindings, wallet).await?),
        ApiRouteKey::transaction => dummy_url_eval(route_pattern, bindings),
        ApiRouteKey::unfreeze => dummy_url_eval(route_pattern, bindings),
        ApiRouteKey::unwrap => response(&req, unwrap(bindings, wallet).await?),
        ApiRouteKey::view => dummy_url_eval(route_pattern, bindings),
        ApiRouteKey::wrap => response(&req, wrap(bindings, wallet).await?),
        ApiRouteKey::getrecords => response(&req, get_records(wallet).await?),
        ApiRouteKey::lastusedkeystore => response(&req, get_last_keystore(path_storage).await?),
    }
}
