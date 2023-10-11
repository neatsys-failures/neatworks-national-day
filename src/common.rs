use std::{collections::HashMap, time::Duration};

use k256::sha2::Digest;
use nix::{
    sched::{sched_setaffinity, CpuSet},
    unistd::Pid,
};
use serde::{Deserialize, Serialize};

use crate::context::{
    crypto::{DigestHash, Hasher},
    ClientIndex, Context, TimerId,
};

#[derive(Debug)]
pub struct Timer {
    pub id: Option<TimerId>,
    duration: Duration,
}

impl Timer {
    pub fn new(duration: Duration) -> Self {
        Self { id: None, duration }
    }

    pub fn set<M>(&mut self, context: &mut Context<M>) {
        let evicted = self.id.replace(context.set(self.duration));
        assert!(evicted.is_none())
    }

    pub fn unset<M>(&mut self, context: &mut Context<M>) {
        context.unset(self.id.take().unwrap())
    }

    pub fn reset<M>(&mut self, context: &mut Context<M>) {
        self.unset(context);
        self.set(context)
    }
}

pub fn set_affinity(index: usize) {
    let mut cpu_set = CpuSet::new();
    cpu_set.set(index).unwrap();
    sched_setaffinity(Pid::from_raw(0), &cpu_set).unwrap()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub client_index: ClientIndex,
    pub request_num: u32,
    pub op: Vec<u8>,
}

impl DigestHash for Request {
    fn hash(&self, hasher: &mut impl std::hash::Hasher) {
        hasher.write_u16(self.client_index);
        hasher.write_u32(self.request_num);
        hasher.write(&self.op)
    }
}

pub type BlockDigest = [u8; 32];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Block {
    pub requests: Vec<Request>,
    pub parent_digest: BlockDigest,
    pub height: u32,
}

impl DigestHash for Block {
    fn hash(&self, hasher: &mut impl std::hash::Hasher) {
        self.requests.hash(hasher);
        hasher.write(&self.parent_digest);
        hasher.write_u32(self.height)
    }
}

impl Block {
    pub fn digest(&self) -> BlockDigest {
        Hasher::sha256(self).finalize().into()
    }
}

#[derive(Debug, Clone)]
pub struct Chain {
    pub digest_parent: BlockDigest,
    height: u32,
    digest_execute: BlockDigest,
    pending_execute: HashMap<BlockDigest, BlockDigest>,
}

impl Chain {
    pub fn new() -> Self {
        Self {
            digest_parent: Self::genesis().digest(),
            height: 0,
            digest_execute: Self::genesis().digest(),
            pending_execute: Default::default(),
        }
    }

    pub fn genesis() -> Block {
        Block::default()
    }
}

impl Default for Chain {
    fn default() -> Self {
        Self::new()
    }
}

impl Chain {
    pub const MAX_BATCH_SIZE: usize = 100;

    pub fn propose(&mut self, requests: &mut Vec<Request>) -> Block {
        assert!(!requests.is_empty());
        self.height += 1;
        let block = Block {
            requests: requests
                .drain(..requests.len().min(Self::MAX_BATCH_SIZE))
                .collect(),
            parent_digest: self.digest_parent,
            height: self.height,
        };
        self.digest_parent = block.digest();
        block
    }

    pub fn commit(&mut self, block: &Block) -> bool {
        if block.parent_digest == self.digest_execute {
            self.digest_execute = block.digest();
            true
        } else {
            let evicted = self
                .pending_execute
                .insert(block.parent_digest, block.digest());
            assert!(evicted.is_none(), "commit conflicting blocks");
            false
        }
    }

    pub fn next_execute(&mut self) -> Option<BlockDigest> {
        if let Some(block_digest) = self.pending_execute.remove(&self.digest_execute) {
            self.digest_execute = block_digest;
            Some(block_digest)
        } else {
            None
        }
    }
}
