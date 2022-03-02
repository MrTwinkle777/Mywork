#![deny(warnings)]

use crate::{mocks::MockCapeLedger, CapeWalletBackend, CapeWalletError};
use address_book::{address_book_port, InsertPubKey};
use async_std::{
    sync::{Arc, Mutex, MutexGuard},
    task::sleep,
};
use async_trait::async_trait;
use cap_rust_sandbox::{
    deploy::EthMiddleware,
    ledger::{CapeLedger, CapeNullifierSet, CapeTransition, CapeTruster},
    model::{Erc20Code, EthereumAddr},
    types::{CAPE, ERC20},
};
use eqs::routes::CapState;
use ethers::{
    core::k256::ecdsa::SigningKey,
    prelude::{
        coins_bip39::English, Address, Http, LocalWallet as LocalEthWallet, MnemonicBuilder,
        Provider, SignerMiddleware, Wallet as EthWallet,
    },
    providers::Middleware,
    signers::Signer,
};
use futures::stream::{self, Stream, StreamExt};
use jf_cap::{
    keys::{UserAddress, UserKeyPair, UserPubKey},
    proof::UniversalParam,
    structs::{AssetDefinition, Nullifier, ReceiverMemo, RecordOpening},
    MerkleTree, Signature,
};
use key_set::ProverKeySet;
use net::client::{parse_error_body, response_body};
use rand_chacha::{rand_core::SeedableRng, ChaChaRng};
use reef::Ledger;
use relayer::SubmitBody;
use seahorse::{
    events::{EventIndex, EventSource, LedgerEvent},
    hd,
    loader::WalletLoader,
    persistence::AtomicWalletStorage,
    txn_builder::{RecordDatabase, TransactionState, TransactionUID},
    WalletBackend, WalletState,
};
use serde::{de::DeserializeOwned, Serialize};
use std::cmp::min;
use std::convert::{TryFrom, TryInto};
use std::pin::Pin;
use std::time::Duration;
use surf::Url;

fn get_provider() -> Provider<Http> {
    let rpc_url = match std::env::var("RPC_URL") {
        Ok(val) => val,
        Err(_) => "http://localhost:8545".to_string(),
    };
    Provider::<Http>::try_from(rpc_url).expect("could not instantiate HTTP Provider")
}

pub struct CapeBackend<'a, Meta: Serialize + DeserializeOwned> {
    universal_param: &'a UniversalParam,
    eqs: surf::Client,
    relayer: surf::Client,
    contract: CAPE<EthMiddleware>,
    storage: Arc<Mutex<AtomicWalletStorage<'a, CapeLedger, Meta>>>,
    key_stream: hd::KeyTree,
    eth_wallet: EthWallet<SigningKey>,
    mock_eqs: Arc<Mutex<MockCapeLedger<'a>>>,
}

impl<'a, Meta: Serialize + DeserializeOwned + Send> CapeBackend<'a, Meta> {
    pub async fn new(
        universal_param: &'a UniversalParam,
        eqs_url: Url,
        relayer_url: Url,
        contract_address: Address,
        eth_mnemonic: Option<String>,
        mock_eqs: Arc<Mutex<MockCapeLedger<'a>>>,
        loader: &mut impl WalletLoader<CapeLedger, Meta = Meta>,
    ) -> Result<CapeBackend<'a, Meta>, CapeWalletError> {
        let eqs: surf::Client = surf::Config::default()
            .set_base_url(eqs_url)
            .try_into()
            .unwrap();
        let eqs = eqs.with(parse_error_body::<relayer::Error>);
        let relayer: surf::Client = surf::Config::default()
            .set_base_url(relayer_url)
            .try_into()
            .unwrap();
        let relayer = relayer.with(parse_error_body::<relayer::Error>);

        // Create an Ethereum wallet to talk to the CAPE contract.
        let provider = get_provider();
        let chain_id = provider.get_chainid().await.unwrap().as_u64();
        // If mnemonic is set, try to use it to create a wallet, otherwise create a random wallet.
        let eth_wallet = match eth_mnemonic {
            Some(mnemonic) => MnemonicBuilder::<English>::default()
                .phrase(mnemonic.as_str())
                .build()
                .map_err(|err| CapeWalletError::Failed {
                    msg: format!("failed to open ETH wallet: {}", err),
                })?,
            None => LocalEthWallet::new(&mut ChaChaRng::from_entropy()),
        }
        .with_chain_id(chain_id);
        let client = Arc::new(SignerMiddleware::new(provider, eth_wallet.clone()));
        let contract = CAPE::new(contract_address, client);

        let storage = AtomicWalletStorage::new(loader, 1024)?;
        let key_stream = storage.key_stream();

        Ok(Self {
            universal_param,
            eqs,
            relayer,
            contract,
            storage: Arc::new(Mutex::new(storage)),
            key_stream,
            eth_wallet,
            mock_eqs,
        })
    }
}

#[async_trait]
impl<'a, Meta: Serialize + DeserializeOwned + Send> WalletBackend<'a, CapeLedger>
    for CapeBackend<'a, Meta>
{
    type Storage = AtomicWalletStorage<'a, CapeLedger, Meta>;
    type EventStream = Pin<Box<dyn Stream<Item = (LedgerEvent<CapeLedger>, EventSource)> + Send>>;

    async fn storage<'l>(&'l mut self) -> MutexGuard<'l, Self::Storage> {
        self.storage.lock().await
    }

    fn key_stream(&self) -> hd::KeyTree {
        self.key_stream.clone()
    }

    async fn submit(
        &mut self,
        txn: CapeTransition,
        uid: TransactionUID<CapeLedger>,
        memos: Vec<ReceiverMemo>,
        sig: Signature,
    ) -> Result<(), CapeWalletError> {
        match &txn {
            CapeTransition::Transaction(txn) => {
                self.relayer
                    .post("submit")
                    .body_json(&SubmitBody {
                        transaction: txn.clone(),
                        memos: memos.clone(),
                        signature: sig.clone(),
                    })
                    .map_err(|err| CapeWalletError::Failed {
                        msg: err.to_string(),
                    })?
                    .send()
                    .await
                    .map_err(|err| CapeWalletError::Failed {
                        msg: format!("relayer error: {}", err),
                    })?;
            }
            CapeTransition::Wrap { .. } => {
                return Err(CapeWalletError::Failed {
                    msg: String::from(
                        "invalid transaction type: wraps must be submitted using `wrap()`, not \
                        `submit()`",
                    ),
                })
            }
        }

        // The mock EQS is not connected to the real contract, so we have to update it by explicitly
        // passing it the submitted transaction. This should not fail, since the submission to the
        // contract above succeeded, and the mock EQS is supposed to be tracking the contract state.
        let mut mock_eqs = self.mock_eqs.lock().await;
        mock_eqs.network().store_call_data(uid, memos, sig);
        mock_eqs.submit(txn).unwrap();

        Ok(())
    }

    async fn create(&mut self) -> Result<WalletState<'a, CapeLedger>, CapeWalletError> {
        let mut res =
            self.eqs
                .get("get_cap_state")
                .send()
                .await
                .map_err(|err| CapeWalletError::Failed {
                    msg: format!("eqs error: {}", err),
                })?;
        let state =
            response_body::<CapState>(&mut res)
                .await
                .map_err(|err| CapeWalletError::Failed {
                    msg: format!("error deserializing eqs response: {}", err),
                })?;

        // `records` should be _almost_ completely sparse. However, even a fully pruned Merkle tree
        // contains the last leaf appended, but as a new wallet, we don't care about _any_ of the
        // leaves, so make a note to forget the last one once more leaves have been appended.
        let record_mt = MerkleTree::restore_from_frontier(
            state.ledger.record_merkle_commitment,
            &state.ledger.record_merkle_frontier,
        )
        .ok_or_else(|| CapeWalletError::Failed {
            msg: String::from("cannot reconstruct Merkle tree from frontier"),
        })?;
        let merkle_leaf_to_forget = if record_mt.num_leaves() > 0 {
            Some(record_mt.num_leaves() - 1)
        } else {
            None
        };

        Ok(WalletState {
            proving_keys: Arc::new(gen_proving_keys(self.universal_param)),
            txn_state: TransactionState {
                validator: CapeTruster::new(state.ledger.state_number, record_mt.num_leaves()),
                now: EventIndex::from_source(EventSource::QueryService, state.num_events as usize),
                // Completely sparse nullifier set
                nullifiers: Default::default(),
                record_mt,
                records: RecordDatabase::default(),
                merkle_leaf_to_forget,
                transactions: Default::default(),
            },
            key_scans: Default::default(),
            key_state: Default::default(),
            auditable_assets: Default::default(),
            audit_keys: Default::default(),
            freeze_keys: Default::default(),
            user_keys: Default::default(),
            defined_assets: Default::default(),
        })
    }

    async fn subscribe(&self, from: EventIndex, to: Option<EventIndex>) -> Self::EventStream {
        // Sleep at least 500ms between each request. This should try to match the EQS polling
        // frequency.
        let min_backoff = Duration::from_millis(500);
        // To avoid overloading the EQS with spurious network traffic, we will increase the backoff
        // time as long as we are not getting any new events, up to a maximum of 1 minute.
        let max_backoff = Duration::from_secs(60);

        struct StreamState {
            from: usize,
            to: Option<usize>,
            eqs: surf::Client,
            backoff: Duration,
            min_backoff: Duration,
            max_backoff: Duration,
        }
        let state = StreamState {
            from: from.index(EventSource::QueryService),
            to: to.map(|to| to.index(EventSource::QueryService)),
            eqs: self.eqs.clone(),
            backoff: min_backoff,
            min_backoff,
            max_backoff,
        };

        Box::pin(
            // Create a stream from a function which polls the EQS. The polling function itself
            // returns a stream of events, since in any given request we may receive more than one
            // event, or zero. Below, we will flatten this stream and tag each event with the event
            // source (which is always QueryService) as required by the WalletBackend API.
            stream::unfold(state, |mut state| async move {
                let req = if let Some(to) = state.to {
                    if state.from >= to {
                        // Returning `None` terminates the stream.
                        return None;
                    }
                    state.eqs.get(&format!(
                        "get_events_since/{}/{}",
                        state.from,
                        to - state.from
                    ))
                } else {
                    state.eqs.get(&format!("get_events_since/{}", state.from))
                };
                let mut res = if let Ok(res) = req.send().await {
                    res
                } else {
                    // Could not connect to EQS. Continue without updating state or yielding any
                    // events, and retry with a backoff.
                    sleep(state.backoff).await;
                    state.backoff = min(state.backoff * 2, state.max_backoff);
                    return Some((stream::iter(vec![]), state));
                };
                // Get events from the response. Panic if the response body does not deserialize
                // properly, as this should never happen as long as the EQS is correct/honest.
                let events = response_body::<Vec<LedgerEvent<CapeLedger>>>(&mut res)
                    .await
                    .unwrap();
                if events.is_empty() {
                    // If there were no new events, increase the backoff before retrying.
                    sleep(state.backoff).await;
                    state.backoff = min(state.backoff * 2, state.max_backoff);
                } else {
                    // If we succeeded in getting new events, reset the backoff.
                    state.backoff = state.min_backoff;
                    // Still sleep for the minimum duration, since we know there will not be new
                    // events at least until the EQS polls again.
                    sleep(state.backoff).await;
                }

                // Update state and yield the events we received.
                state.from += events.len();
                Some((stream::iter(events), state))
            })
            .flatten()
            .map(|event| (event, EventSource::QueryService)),
        )
    }

    ////////////////////////////////////////////////////////////////////////////////////////////////
    // The remaining backend methods are still mocked, pending completion of the EQS
    //

    async fn get_public_key(&self, address: &UserAddress) -> Result<UserPubKey, CapeWalletError> {
        let address_bytes = bincode::serialize(address).unwrap();
        let mut response = surf::post(format!(
            "http://localhost:{}/request_pubkey",
            address_book_port()
        ))
        .content_type(surf::http::mime::BYTE_STREAM)
        .body_bytes(&address_bytes)
        .await
        .map_err(|err| CapeWalletError::Failed {
            msg: format!("error requesting public key: {}", err),
        })?;
        let bytes = response.body_bytes().await.unwrap();
        let pub_key: UserPubKey = bincode::deserialize(&bytes).unwrap();
        Ok(pub_key)
    }

    async fn get_nullifier_proof(
        &self,
        nullifiers: &mut CapeNullifierSet,
        nullifier: Nullifier,
    ) -> Result<(bool, ()), CapeWalletError> {
        // Try to look up the nullifier in our local cache. If it is not there, query the contract
        // and cache it.
        match nullifiers.get(nullifier) {
            Some(ret) => Ok((ret, ())),
            None => {
                let ret = self
                    .mock_eqs
                    .lock()
                    .await
                    .network()
                    .nullifier_spent(nullifier);
                nullifiers.insert(nullifier, ret);
                Ok((ret, ()))
            }
        }
    }

    async fn get_transaction(
        &self,
        block_id: u64,
        txn_id: u64,
    ) -> Result<CapeTransition, CapeWalletError> {
        self.mock_eqs
            .lock()
            .await
            .network()
            .get_transaction(block_id, txn_id)
    }

    async fn register_user_key(&mut self, key_pair: &UserKeyPair) -> Result<(), CapeWalletError> {
        let pub_key_bytes = bincode::serialize(&key_pair.pub_key()).unwrap();
        let sig = key_pair.sign(&pub_key_bytes);
        let json_request = InsertPubKey { pub_key_bytes, sig };
        match surf::post(format!(
            "http://localhost:{}/insert_pubkey",
            address_book_port()
        ))
        .content_type(surf::http::mime::JSON)
        .body_json(&json_request)
        .unwrap()
        .await
        {
            Ok(_) => Ok(()),
            Err(err) => Err(CapeWalletError::Failed {
                msg: format!("error inserting public key: {}", err),
            }),
        }
    }
}

#[async_trait]
impl<'a, Meta: Serialize + DeserializeOwned + Send> CapeWalletBackend<'a>
    for CapeBackend<'a, Meta>
{
    async fn register_erc20_asset(
        &mut self,
        asset: &AssetDefinition,
        erc20_code: Erc20Code,
        sponsor: EthereumAddr,
    ) -> Result<(), CapeWalletError> {
        self.contract
            .sponsor_cape_asset(erc20_code.clone().into(), asset.clone().into())
            .from(Address::from(sponsor.clone()))
            .send()
            .await
            .map_err(|err| CapeWalletError::Failed {
                msg: format!("error building CAPE::sponsorCapeAsset transaction: {}", err),
            })?
            .await
            .map_err(|err| CapeWalletError::Failed {
                msg: format!(
                    "error submitting CAPE::sponsorCapeAsset transaction: {}",
                    err
                ),
            })?;

        // Update the mock EQS to correspond with the new state of the contract. This cannot fail
        // since the update to the real contract succeeded.
        self.mock_eqs
            .lock()
            .await
            .network()
            .register_erc20(asset.clone(), erc20_code, sponsor)
            .unwrap();

        Ok(())
    }

    async fn get_wrapped_erc20_code(
        &self,
        asset: &AssetDefinition,
    ) -> Result<Erc20Code, CapeWalletError> {
        self.mock_eqs
            .lock()
            .await
            .network()
            .get_wrapped_asset(asset)
    }

    async fn wrap_erc20(
        &mut self,
        erc20_code: Erc20Code,
        src_addr: EthereumAddr,
        ro: RecordOpening,
    ) -> Result<(), CapeWalletError> {
        // Before the contract can transfer from our account, in accordance with the ERC20 protocol,
        // we have to approve the transfer.
        ERC20::new(erc20_code.clone(), self.eth_client().unwrap())
            .approve(self.contract.address(), ro.amount.into())
            .send()
            .await
            .map_err(|err| CapeWalletError::Failed {
                msg: format!("error building ERC20::approve transaction: {}", err),
            })?
            .await
            .map_err(|err| CapeWalletError::Failed {
                msg: format!("error submitting ERC20::approve transaction: {}", err),
            })?;

        // Wraps don't go through the relayer, they go directly to the contract.
        self.contract
            .deposit_erc_20(ro.clone().into(), erc20_code.clone().into())
            .from(Address::from(src_addr.clone()))
            .send()
            .await
            .map_err(|err| CapeWalletError::Failed {
                msg: format!("error building CAPE::depositErc20 transaction: {}", err),
            })?
            .await
            .map_err(|err| CapeWalletError::Failed {
                msg: format!("error submitting CAPE::depositErc20 transaction: {}", err),
            })?;

        // The mock EQS is not connected to the real contract, so we have to update it by explicitly
        // passing it the submitted transaction. This should not fail, since the submission to the
        // contract above succeded, and the mock EQS is supposed to be tracking the contract state.
        let mut mock_eqs = self.mock_eqs.lock().await;
        mock_eqs
            .submit(CapeTransition::Wrap {
                ro: Box::new(ro),
                erc20_code,
                src_addr,
            })
            .unwrap();

        Ok(())
    }

    fn eth_client(&self) -> Result<Arc<EthMiddleware>, CapeWalletError> {
        Ok(Arc::new(SignerMiddleware::new(
            get_provider(),
            self.eth_wallet.clone(),
        )))
    }
}

const TRANSFER_KEY_SIZES: &[(usize, usize)] = &[(1, 2), (2, 2), (2, 3)];
const FREEZE_KEY_SIZES: &[usize] = &[2, 3];

fn gen_proving_keys(srs: &UniversalParam) -> ProverKeySet<key_set::OrderByOutputs> {
    use jf_cap::proof::{freeze, mint, transfer};

    ProverKeySet {
        mint: mint::preprocess(srs, CapeLedger::merkle_height())
            .unwrap()
            .0,
        xfr: TRANSFER_KEY_SIZES
            .iter()
            .map(|(inputs, outputs)| {
                transfer::preprocess(srs, *inputs, *outputs, CapeLedger::merkle_height())
                    .unwrap()
                    .0
            })
            .collect(),
        freeze: FREEZE_KEY_SIZES
            .iter()
            .map(|inputs| {
                freeze::preprocess(srs, *inputs, CapeLedger::merkle_height())
                    .unwrap()
                    .0
            })
            .collect(),
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::testing::{
        create_test_network, spawn_eqs, sponsor_simple_token, transfer_token, wrap_simple_token,
    };
    use crate::{mocks::MockCapeWalletLoader, CapeWallet, CapeWalletExt};
    use cap_rust_sandbox::{deploy::deploy_erc20_token, universal_param::UNIVERSAL_PARAM};
    use ethers::types::{TransactionRequest, U256};
    use jf_cap::structs::AssetCode;
    use rand_chacha::{rand_core::SeedableRng, ChaChaRng};
    use seahorse::testing::await_transaction;
    use std::path::PathBuf;
    use std::time::Duration;
    use tempdir::TempDir;

    #[async_std::test]
    async fn test_transfer() {
        let mut rng = ChaChaRng::from_seed([1u8; 32]);
        let universal_param = &*UNIVERSAL_PARAM;
        let (sender_key, relayer_url, contract_address, mock_eqs) =
            create_test_network(&mut rng, universal_param).await;
        let (eqs_url, _eqs_dir, _join_eqs) = spawn_eqs(contract_address).await;

        // Create a sender wallet and add the key pair that owns the faucet record.
        let sender_dir = TempDir::new("cape_wallet_backend_test").unwrap();
        let mut sender_loader = MockCapeWalletLoader {
            path: PathBuf::from(sender_dir.path()),
            key: hd::KeyTree::random(&mut rng).unwrap().0,
        };
        let sender_backend = CapeBackend::new(
            universal_param,
            eqs_url.clone(),
            relayer_url.clone(),
            contract_address,
            None,
            mock_eqs.clone(),
            &mut sender_loader,
        )
        .await
        .unwrap();
        let mut sender = CapeWallet::new(sender_backend).await.unwrap();
        sender
            .add_user_key(sender_key.clone(), EventIndex::default())
            .await
            .unwrap();
        // Wait for the wallet to register the balance belonging to the key, from the initial grant
        // records. Depending on exactly when the key was added relative to the background event
        // handling thread, the relevant events may be processed by either the event handling thread
        // or the key scan thread, so we will wait for both of them to complete.
        sender.sync(mock_eqs.lock().await.now()).await.unwrap();
        sender.await_key_scan(&sender_key.address()).await.unwrap();
        let total_balance = sender
            .balance(&sender_key.address(), &AssetCode::native())
            .await;
        assert!(total_balance > 0);

        // Create an empty receiver wallet, and generating a receiving key.
        let receiver_dir = TempDir::new("cape_wallet_backend_test").unwrap();
        let mut receiver_loader = MockCapeWalletLoader {
            path: PathBuf::from(receiver_dir.path()),
            key: hd::KeyTree::random(&mut rng).unwrap().0,
        };
        let receiver_backend = CapeBackend::new(
            universal_param,
            eqs_url.clone(),
            relayer_url.clone(),
            contract_address,
            None,
            mock_eqs.clone(),
            &mut receiver_loader,
        )
        .await
        .unwrap();
        let mut receiver = CapeWallet::new(receiver_backend).await.unwrap();
        let receiver_key = receiver.generate_user_key(None).await.unwrap();

        // Transfer from sender to receiver.
        let txn = transfer_token(
            &mut sender,
            receiver_key.address(),
            2,
            AssetCode::native(),
            1,
        )
        .await
        .unwrap();
        await_transaction(&txn, &sender, &[&receiver]).await;
        assert_eq!(
            sender
                .balance(&sender_key.address(), &AssetCode::native())
                .await,
            total_balance - 3
        );
        assert_eq!(
            receiver
                .balance(&receiver_key.address(), &AssetCode::native())
                .await,
            2
        );

        // Transfer back, just to make sure the receiver is actually able to spend the records it
        // received.
        let txn = transfer_token(
            &mut receiver,
            sender_key.address(),
            1,
            AssetCode::native(),
            1,
        )
        .await
        .unwrap();
        await_transaction(&txn, &receiver, &[&sender]).await;
        assert_eq!(
            sender
                .balance(&sender_key.address(), &AssetCode::native())
                .await,
            total_balance - 2
        );
        assert_eq!(
            receiver
                .balance(&receiver_key.address(), &AssetCode::native())
                .await,
            0
        );
    }

    #[async_std::test]
    async fn test_anonymous_erc20_transfer() {
        let mut rng = ChaChaRng::from_seed([1u8; 32]);
        let universal_param = &*UNIVERSAL_PARAM;
        let (wrapper_key, relayer_url, contract_address, mock_eqs) =
            create_test_network(&mut rng, universal_param).await;
        let (eqs_url, _eqs_dir, _join_eqs) = spawn_eqs(contract_address).await;

        // Create a wallet to sponsor an asset and a different wallet to deposit (we should be able
        // to deposit from an account other than the sponsor).
        let sponsor_dir = TempDir::new("cape_wallet_backend_test").unwrap();
        let mut sponsor_loader = MockCapeWalletLoader {
            path: PathBuf::from(sponsor_dir.path()),
            key: hd::KeyTree::random(&mut rng).unwrap().0,
        };
        let sponsor_backend = CapeBackend::new(
            universal_param,
            eqs_url.clone(),
            relayer_url.clone(),
            contract_address.clone(),
            None,
            mock_eqs.clone(),
            &mut sponsor_loader,
        )
        .await
        .unwrap();
        let mut sponsor = CapeWallet::new(sponsor_backend).await.unwrap();
        let sponsor_key = sponsor.generate_user_key(None).await.unwrap();
        let sponsor_eth_addr = sponsor.eth_address().await.unwrap();

        let wrapper_dir = TempDir::new("cape_wallet_backend_test").unwrap();
        let mut wrapper_loader = MockCapeWalletLoader {
            path: PathBuf::from(wrapper_dir.path()),
            key: hd::KeyTree::random(&mut rng).unwrap().0,
        };
        let wrapper_backend = CapeBackend::new(
            universal_param,
            eqs_url.clone(),
            relayer_url.clone(),
            contract_address.clone(),
            None,
            mock_eqs.clone(),
            &mut wrapper_loader,
        )
        .await
        .unwrap();
        let mut wrapper = CapeWallet::new(wrapper_backend).await.unwrap();
        // Add the faucet key to the wrapper wallet, so that they have the native tokens they need
        // to pay the fee to transfer the wrapped tokens.
        wrapper
            .add_user_key(wrapper_key.clone(), EventIndex::default())
            .await
            .unwrap();
        // Wait for the wrapper to register the balance belonging to the key, from the initial grant
        // records. Depending on exactly when the key was added relative to the background event
        // handling thread, the relevant events may be processed by either the event handling thread
        // or the key scan thread, so we will wait for both of them to complete.
        wrapper.sync(mock_eqs.lock().await.now()).await.unwrap();
        wrapper
            .await_key_scan(&wrapper_key.address())
            .await
            .unwrap();
        let total_native_balance = wrapper
            .balance(&wrapper_key.address(), &AssetCode::native())
            .await;
        assert!(total_native_balance > 0);

        // Fund the Ethereum wallets for contract calls.
        let provider = get_provider().interval(Duration::from_millis(100u64));
        let accounts = provider.get_accounts().await.unwrap();
        assert!(!accounts.is_empty());
        for wallet in [&sponsor, &wrapper] {
            let tx = TransactionRequest::new()
                .to(Address::from(wallet.eth_address().await.unwrap()))
                .value(ethers::utils::parse_ether(U256::from(1)).unwrap())
                .from(accounts[0]);
            provider
                .send_transaction(tx, None)
                .await
                .unwrap()
                .await
                .unwrap();
        }

        let erc20_contract = deploy_erc20_token().await;

        // Sponsor a CAPE asset corresponding to an ERC20 token.
        let cape_asset = sponsor_simple_token(&mut sponsor, &erc20_contract)
            .await
            .unwrap();

        wrap_simple_token(
            &mut wrapper,
            &wrapper_key.address(),
            cape_asset.clone(),
            &erc20_contract,
            100,
        )
        .await
        .unwrap();

        // To force the wrap to be processed, we need to submit a block of CAPE transactions. We'll
        // transfer some native tokens from `wrapper` to `sponsor`.
        let receipt = wrapper
            .transfer(
                &wrapper_key.address(),
                &AssetCode::native(),
                &[(sponsor_key.address(), 1)],
                1,
            )
            .await
            .unwrap();
        await_transaction(&receipt, &wrapper, &[&sponsor]).await;
        assert_eq!(
            wrapper
                .balance(&wrapper_key.address(), &AssetCode::native())
                .await,
            total_native_balance - 2
        );
        assert_eq!(
            sponsor
                .balance(&sponsor_key.address(), &AssetCode::native())
                .await,
            1
        );
        // The transfer transaction caused the wrap record to be created.
        assert_eq!(
            wrapper
                .balance(&wrapper_key.address(), &cape_asset.code)
                .await,
            100
        );

        // Make sure the wrapper can access the wrapped tokens, by transferring them to someone else
        // (we'll reuse the `sponsor` wallet, but this could be a separate role).
        let receipt = wrapper
            .transfer(
                &wrapper_key.address(),
                &cape_asset.code,
                &[(sponsor_key.address(), 100)],
                1,
            )
            .await
            .unwrap();
        await_transaction(&receipt, &wrapper, &[&sponsor]).await;
        assert_eq!(
            wrapper
                .balance(&wrapper_key.address(), &cape_asset.code)
                .await,
            0
        );
        assert_eq!(
            sponsor
                .balance(&sponsor_key.address(), &cape_asset.code)
                .await,
            100
        );

        // Finally, withdraw the wrapped tokens back into the ERC20 token type.
        let receipt = sponsor
            .burn(
                &sponsor_key.address(),
                sponsor_eth_addr.clone().into(),
                &cape_asset.code,
                100,
                1,
            )
            .await
            .unwrap();
        await_transaction(&receipt, &sponsor, &[]).await;
        assert_eq!(
            sponsor
                .balance(&sponsor_key.address(), &cape_asset.code)
                .await,
            0
        );
        assert_eq!(
            erc20_contract
                .balance_of(sponsor_eth_addr.into())
                .call()
                .await
                .unwrap(),
            100.into()
        );
    }
}
