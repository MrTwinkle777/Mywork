// Copyright (c) 2022 Espresso Systems (espressosys.com)
// This file is part of the Configurable Asset Privacy for Ethereum (CAPE) library.

// This program is free software: you can redistribute it and/or modify it under the terms of the GNU General Public License as published by the Free Software Foundation, either version 3 of the License, or (at your option) any later version.
// This program is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the GNU General Public License for more details.
// You should have received a copy of the GNU General Public License along with this program. If not, see <https://www.gnu.org/licenses/>.

use crate::configuration::EQSOptions;
use crate::query_result_state::QueryResultState;
use crate::state_persistence::StatePersistence;

use async_std::sync::{Arc, RwLock};
use cap_rust_sandbox::{
    cape::submit_block::fetch_cape_block,
    ethereum::EthConnection,
    ledger::CapeTransition,
    model::{CapeModelTxn, Erc20Code, EthereumAddr},
    types::{CAPEEvents, RecordOpening as RecordOpeningSol},
};
use commit::Committable;
use core::mem;
use ethers::abi::AbiDecode;
use jf_cap::structs::{ReceiverMemo, RecordCommitment};
use jf_cap::{structs::RecordOpening, MerkleTree, TransactionNote};
use rand_chacha::rand_core::SeedableRng;
use rand_chacha::ChaChaRng;
use reef::traits::{Block, Transaction};
use seahorse::events::LedgerEvent;

pub(crate) struct EthPolling {
    pub query_result_state: Arc<RwLock<QueryResultState>>,
    pub state_persistence: StatePersistence,
    pub last_updated_block_height: u64,
    pub pending_commit_event: Vec<CapeTransition>,
    pub connection: EthConnection,
}

impl EthPolling {
    pub async fn new(
        opt: &EQSOptions,
        query_result_state: Arc<RwLock<QueryResultState>>,
        state_persistence: StatePersistence,
    ) -> EthPolling {
        if opt.temp_test_run() {
            return EthPolling {
                query_result_state,
                state_persistence,
                last_updated_block_height: 0u64,
                pending_commit_event: Vec::new(),
                connection: EthConnection::for_test().await,
            };
        }

        let (connection, last_updated_block_height) = if let Some(contract_address) =
            opt.cape_address()
        {
            let mut state_updater = query_result_state.write().await;
            let last_updated_block_height = state_updater.last_updated_block_height;

            if state_updater.contract_address.is_none()
                && state_updater.last_updated_block_height > 0
            {
                panic!(
                    "Persisted state is malformed! Run again with --reset_store_state to repair"
                );
            }

            if let Some(persisted_contract_address) = state_updater.contract_address {
                if persisted_contract_address != contract_address {
                    panic!("The specified persisted state was generated for a different contract.");
                }
            } else {
                state_updater.contract_address = Some(contract_address);
            }

            (
                EthConnection::from_config_for_query(
                    &format!("{:?}", contract_address),
                    opt.rpc_url(),
                ),
                last_updated_block_height,
            )
        } else {
            panic!("Invocation Error! Address required unless launched for testing");
        };

        EthPolling {
            query_result_state,
            state_persistence,
            last_updated_block_height,
            pending_commit_event: Vec::new(),
            connection,
        }
    }

    pub async fn check(&mut self) -> Result<u64, async_std::io::Error> {
        // do eth poll, unpack updates
        //select cape events, last block + 1 to avoid grabbing the same event twice
        let new_event = self
            .connection
            .contract
            .events()
            .from_block(self.last_updated_block_height + 1)
            .query_with_meta()
            .await
            .unwrap();

        for (filter, meta) in new_event {
            match filter {
                CAPEEvents::BlockCommittedFilter(_) => {
                    let fetched_block_with_memos =
                        fetch_cape_block(&self.connection, meta.transaction_hash)
                            .await
                            .unwrap()
                            .unwrap();

                    let model_txns = fetched_block_with_memos
                        .block
                        .clone()
                        .into_cape_transactions()
                        .unwrap()
                        .0;

                    // TODO Instead of panicking here we need to handle cases of missing memos gracefully
                    let num_txn = model_txns.len();
                    let num_txn_memo = fetched_block_with_memos.memos.len();
                    if num_txn != num_txn_memo {
                        panic!(
                            "Different number of txns and txn memos: {} vs {}",
                            num_txn, num_txn_memo
                        );
                    }

                    let mut wraps = mem::take(&mut self.pending_commit_event);

                    //add transactions followed by wraps to pending commit
                    let mut transitions = Vec::new();
                    for tx in model_txns.clone() {
                        transitions.push(CapeTransition::Transaction(tx.clone()));
                    }

                    self.pending_commit_event.append(&mut transitions.clone());
                    self.pending_commit_event.append(&mut wraps);

                    let output_record_commitments = fetched_block_with_memos
                        .block
                        .get_list_of_output_record_commitments();

                    let state_lock = self.query_result_state.read().await;
                    let merkle_tree = MerkleTree::restore_from_frontier(
                        state_lock.ledger_state.record_merkle_commitment,
                        &state_lock.ledger_state.record_merkle_frontier,
                    );

                    //add commitments to merkle tree
                    let mut uids = Vec::new();
                    let mut merkle_paths = Vec::new();
                    if let Some(mut merkle_tree) = merkle_tree.clone() {
                        for (_record_id, record_commitment) in
                            output_record_commitments.iter().enumerate()
                        {
                            uids.push(merkle_tree.num_leaves());
                            merkle_tree.push(record_commitment.to_field_element());
                        }
                        merkle_paths = uids
                            .iter()
                            .map(|uid| merkle_tree.get_leaf(*uid).expect_ok().unwrap().1.path)
                            .collect::<Vec<_>>();
                    }

                    let memos_sig_valid: Vec<_> = model_txns
                        .iter()
                        .zip(fetched_block_with_memos.memos.iter())
                        .map(|(tx, (recv_memos, sig))| {
                            match tx {
                                CapeModelTxn::CAP(note) => note.clone(),
                                CapeModelTxn::Burn { xfr, .. } => {
                                    TransactionNote::from(*xfr.clone())
                                }
                            }
                            .verify_receiver_memos_signature(recv_memos, sig)
                            .is_ok()
                        })
                        .collect();

                    // Create LedgerEvent::Memos if memo signature is valid, skip otherwise
                    let mut memo_events = Vec::new();
                    let mut index = 0;
                    fetched_block_with_memos
                        .memos
                        .iter()
                        .enumerate()
                        .filter_map(|(txn_id, (txn_memo, _))| match memos_sig_valid[txn_id] {
                            true => Some((txn_id, txn_memo)),
                            false => None,
                        })
                        .for_each(|(txn_id, txn_memo)| {
                            let mut outputs = Vec::new();
                            for memo in txn_memo.iter() {
                                outputs.push((
                                    memo.clone(),
                                    output_record_commitments[index],
                                    uids[index],
                                    merkle_paths[index].clone(),
                                ));
                                index += 1;
                            }
                            let memo_event = LedgerEvent::Memos {
                                outputs,
                                transaction: Some((
                                    meta.block_number.as_u64(),
                                    txn_id as u64,
                                    transitions[txn_id].kind(),
                                )),
                            };
                            memo_events.push(memo_event);
                        });
                    let new_updated_block_height = meta.block_number.as_u64();
                    if new_updated_block_height > self.last_updated_block_height {
                        let mut updated_state = self.query_result_state.write().await;
                        // update the state block
                        let pending_commit = mem::take(&mut self.pending_commit_event);

                        //create/push pending commit to QueryResultState events
                        updated_state.events.push(LedgerEvent::Commit {
                            block: cap_rust_sandbox::ledger::CapeBlock::new(pending_commit),
                            block_id: meta.block_number.as_u64(),
                            state_comm: meta.block_number.as_u64() + 1,
                        });

                        updated_state.events.append(&mut memo_events);
                        updated_state.ledger_state.state_number += 1;

                        //update merkle tree
                        if let Some(merkle_tree) = merkle_tree {
                            updated_state.ledger_state.record_merkle_commitment =
                                merkle_tree.commitment();
                            updated_state.ledger_state.record_merkle_frontier =
                                merkle_tree.frontier();
                        }

                        //update transaction_by_id and transaction_id_by_hash hashmap
                        let mut record_index = 0;
                        transitions
                            .iter()
                            .enumerate()
                            .for_each(|(txn_id, transition)| {
                                updated_state.transaction_by_id.insert(
                                    (meta.block_number.as_u64(), txn_id as u64),
                                    cap_rust_sandbox::ledger::CommittedCapeTransition {
                                        block_id: meta.block_number.as_u64(),
                                        txn_id: txn_id as u64,
                                        output_start: uids[record_index],
                                        output_size: transition.output_len() as u64,
                                        transition: transition.clone(),
                                    },
                                );
                                updated_state.transaction_id_by_hash.insert(
                                    transition.commit(),
                                    (meta.block_number.as_u64(), txn_id as u64),
                                );
                                for nullifier in transition.proven_nullifiers().iter() {
                                    updated_state.nullifiers.insert(nullifier.0);
                                }

                                record_index += transition.output_len();
                            });

                        // persist the state block updates (will be more fine grained in r3)
                        self.state_persistence.store_latest_state(&*updated_state);
                    }
                }

                CAPEEvents::Erc20TokensDepositedFilter(filter_data) => {
                    let ro_bytes = filter_data.ro_bytes.clone();
                    let ro_sol: RecordOpeningSol = AbiDecode::decode(ro_bytes).unwrap();
                    let expected_ro = RecordOpening::from(ro_sol);

                    let erc20_code = Erc20Code(EthereumAddr(
                        filter_data.erc_20_token_address.to_fixed_bytes(),
                    ));

                    let new_transition_wrap = CapeTransition::Wrap {
                        ro: Box::new(expected_ro),
                        erc20_code,
                        src_addr: EthereumAddr(filter_data.from.to_fixed_bytes()),
                    };
                    self.pending_commit_event.push(new_transition_wrap);
                }
                CAPEEvents::FaucetInitializedFilter(filter_data) => {
                    let ro_bytes = filter_data.ro_bytes;

                    // Obtain record opening
                    let ro_sol: RecordOpeningSol = AbiDecode::decode(ro_bytes).unwrap();
                    let ro = RecordOpening::from(ro_sol);

                    // Compute record commmitment
                    let rc = RecordCommitment::from(&ro);

                    // Compute memo
                    let mut rng = ChaChaRng::from_entropy();
                    let memo = ReceiverMemo::from_ro(&mut rng, &ro, &[]).unwrap();

                    // Update the Merkle tree
                    let state_lock = self.query_result_state.read().await;
                    let merkle_tree = MerkleTree::restore_from_frontier(
                        state_lock.ledger_state.record_merkle_commitment,
                        &state_lock.ledger_state.record_merkle_frontier,
                    );

                    let mut merkle_tree = merkle_tree.unwrap().clone();
                    let uid = merkle_tree.num_leaves();
                    merkle_tree.push(rc.to_field_element());
                    let merkle_path = merkle_tree.get_leaf(uid).expect_ok().unwrap().1.path;

                    // Process the memo
                    let output = (memo.clone(), rc, uid, merkle_path.clone());

                    let memo_event = LedgerEvent::Memos {
                        outputs: vec![output],
                        transaction: None,
                    };

                    let mut memo_events = vec![memo_event];

                    // Update the local data structures
                    let mut updated_state = self.query_result_state.write().await;

                    updated_state.ledger_state.record_merkle_commitment = merkle_tree.commitment();
                    updated_state.ledger_state.record_merkle_frontier = merkle_tree.frontier();

                    updated_state.events.append(&mut memo_events);

                    // persist the state block updates (will be more fine grained in r3)
                    self.state_persistence.store_latest_state(&*updated_state);
                }
            }
        }
        Ok(0)
    }
}
