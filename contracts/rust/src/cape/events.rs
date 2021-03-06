// Copyright (c) 2022 Espresso Systems (espressosys.com)
// This file is part of the Configurable Asset Privacy for Ethereum (CAPE) library.

// This program is free software: you can redistribute it and/or modify it under the terms of the GNU General Public License as published by the Free Software Foundation, either version 3 of the License, or (at your option) any later version.
// This program is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the GNU General Public License for more details.
// You should have received a copy of the GNU General Public License along with this program. If not, see <https://www.gnu.org/licenses/>.

#[cfg(test)]
mod tests {
    use crate::{
        cape::{
            submit_block::{fetch_cape_block, submit_cape_block_with_memos},
            BlockWithMemos, CapeBlock,
        },
        ethereum::EthConnection,
        ledger::CapeLedger,
        types::{GenericInto, MerkleRootSol},
    };
    use anyhow::Result;
    use itertools::Itertools;
    use jf_cap::KeyPair;
    use jf_cap::{
        keys::{UserKeyPair, UserPubKey},
        sign_receiver_memos,
        structs::{AssetDefinition, FreezeFlag, ReceiverMemo, RecordOpening},
        utils::TxnsParams,
    };
    use rand::{RngCore, SeedableRng};
    use rand_chacha::ChaChaRng;
    use reef::Ledger;
    use std::iter::repeat_with;

    #[tokio::test]
    async fn test_fetch_cape_block_from_event() -> Result<()> {
        let connection = EthConnection::for_test().await;

        let mut rng = ChaChaRng::from_seed([0x42u8; 32]);
        let params = TxnsParams::generate_txns(&mut rng, 1, 0, 0, CapeLedger::merkle_height());
        let miner = UserPubKey::default();

        let root = params.txns[0].merkle_root();

        let cape_block = CapeBlock::generate(params.txns, vec![], miner.address())?;

        connection
            .test_contract()
            .add_root(root.generic_into::<MerkleRootSol>().0)
            .send()
            .await?
            .await?;

        // XXX Adapted from seahorse
        // https://github.com/EspressoSystems/seahorse/blob/ace20bc5f1bcf5b88ca0562799b8e80e6c52e933/src/persistence.rs#L574
        // Generate some memos with default UserKeyPair
        let key_pair = UserKeyPair::default();
        let memos_with_sigs = repeat_with(|| {
            let memos = repeat_with(|| {
                let amount = rng.next_u64();
                let ro = RecordOpening::new(
                    &mut rng,
                    amount,
                    AssetDefinition::native(),
                    key_pair.pub_key(),
                    FreezeFlag::Unfrozen,
                );
                ReceiverMemo::from_ro(&mut rng, &ro, &[]).unwrap()
            })
            .take(3)
            .collect::<Vec<_>>();
            let sig = sign_receiver_memos(&KeyPair::generate(&mut rng), &memos).unwrap();
            (memos, sig)
        })
        .take(3)
        .collect_vec();

        let block_with_memos = BlockWithMemos::new(cape_block.clone(), memos_with_sigs.clone());

        submit_cape_block_with_memos(&connection.contract, block_with_memos.clone())
            .await?
            .await?;

        // A connection with a random wallet (for queries only)
        let query_connection = EthConnection::from_config_for_query(
            &format!("{:?}", connection.contract.address()), // 0x123...cdf
            "http://localhost:8545",
        );

        let events = query_connection
            .contract
            .block_committed_filter()
            .from_block(0u64)
            .query_with_meta()
            .await?;

        let (_, meta) = events[0].clone();

        let fetched_block_with_memos = fetch_cape_block(&query_connection, meta.transaction_hash)
            .await?
            .unwrap();

        assert_eq!(fetched_block_with_memos, block_with_memos);

        Ok(())
    }
}
