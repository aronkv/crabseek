//! Our place in the distributed search network, as a child.
//!
//! After login we tell the server we have no parent; it answers with
//! `PossibleParents`. We connect to them (`D` connections) and adopt the
//! first one that sends a search after telling us its branch level. Then
//! we report our level and root to the server. Searches from the parent,
//! or embedded in server messages when we are a branch root ourselves, are
//! answered from the share index. We do not take children (yet).

use std::collections::HashMap;

use crabseek_proto::ConnectionType;
use crabseek_proto::distrib::DistribMsg;
use crabseek_proto::server::{PossibleParent, ServerRequest};
use tokio::net::TcpStream;

use super::{Actor, Event, Purpose};
use crate::distrib_conn::{self, DistribHandle};
use crate::peer::ConnId;

/// How we currently take part in the distributed network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DistribStatus {
    /// Looking for a parent.
    Searching,
    Parent {
        username: String,
        level: i32,
    },
    /// The server sends us searches directly.
    BranchRoot,
}

struct Candidate {
    handle: DistribHandle,
    level: Option<i32>,
    root: Option<String>,
}

struct Parent {
    username: String,
    handle: DistribHandle,
    level: i32,
    root: String,
}

#[derive(Default)]
pub(super) struct Distrib {
    parent: Option<Parent>,
    candidates: HashMap<String, Candidate>,
    branch_root: bool,
}

impl Actor {
    fn distrib_status(&self) -> DistribStatus {
        match (&self.distrib.parent, self.distrib.branch_root) {
            (Some(p), _) => DistribStatus::Parent {
                username: p.username.clone(),
                level: p.level,
            },
            (None, true) => DistribStatus::BranchRoot,
            (None, false) => DistribStatus::Searching,
        }
    }

    fn emit_distrib_status(&self) {
        self.emit(Event::Distrib(self.distrib_status()));
    }

    pub(super) async fn on_possible_parents(&mut self, parents: Vec<PossibleParent>) {
        if self.distrib.parent.is_some() {
            return;
        }
        for p in parents {
            if p.username == self.own_username || self.distrib.candidates.contains_key(&p.username)
            {
                continue;
            }
            self.start_connect_known(
                p.username,
                p.ip,
                p.port,
                ConnectionType::Distributed,
                Purpose::ParentCandidate,
            )
            .await;
        }
    }

    /// A `D` connection to a possible parent is open.
    pub(super) fn on_candidate_connected(&mut self, username: String, stream: TcpStream) {
        if self.distrib.parent.is_some() || self.distrib.candidates.contains_key(&username) {
            return;
        }
        self.next_conn_id += 1;
        let handle = distrib_conn::spawn(
            self.next_conn_id,
            username.clone(),
            stream,
            self.internal.clone(),
        );
        tracing::debug!(%username, "connected to a possible distributed parent");
        self.distrib.candidates.insert(
            username,
            Candidate {
                handle,
                level: None,
                root: None,
            },
        );
    }

    pub(super) async fn on_distrib_message(
        &mut self,
        id: ConnId,
        username: String,
        msg: DistribMsg,
    ) {
        // Unwrap searches that older SoulseekQt versions embed.
        let msg = match msg {
            DistribMsg::Embedded { code, payload } => {
                match DistribMsg::decode_body(code, payload) {
                    Ok(inner) => inner,
                    Err(e) => {
                        tracing::debug!(%username, %e, "bad embedded distributed message");
                        return;
                    }
                }
            }
            msg => msg,
        };

        let from_parent = self
            .distrib
            .parent
            .as_ref()
            .is_some_and(|p| p.username == username && p.handle.id == id);
        if from_parent {
            self.on_parent_message(msg).await;
            return;
        }

        let Some(candidate) = self
            .distrib
            .candidates
            .get_mut(&username)
            .filter(|c| c.handle.id == id)
        else {
            return;
        };
        match msg {
            DistribMsg::BranchLevel(level) => candidate.level = Some(level),
            DistribMsg::BranchRoot(root) => candidate.root = Some(root),
            // The first candidate that searches after telling us its level
            // becomes our parent.
            DistribMsg::Search {
                username: searcher,
                token,
                query,
            } if candidate.level.is_some() => {
                self.adopt_parent(&username).await;
                self.on_search_request(searcher, token, query).await;
            }
            _ => {}
        }
    }

    async fn adopt_parent(&mut self, username: &str) {
        let Some(candidate) = self.distrib.candidates.remove(username) else {
            return;
        };
        let level = candidate.level.unwrap_or(0);
        let root = match candidate.root {
            Some(root) => root,
            // A level-0 parent is the root itself.
            None => username.to_owned(),
        };
        // The other candidates close when dropped.
        self.distrib.candidates.clear();
        self.distrib.branch_root = false;
        tracing::info!(parent = %username, level, %root, "adopted distributed parent");
        self.distrib.parent = Some(Parent {
            username: username.to_owned(),
            handle: candidate.handle,
            level,
            root: root.clone(),
        });
        self.report_branch().await;
        self.emit_distrib_status();
    }

    async fn report_branch(&mut self) {
        let Some(p) = &self.distrib.parent else {
            return;
        };
        let (level, root) = ((p.level + 1).max(0) as u32, p.root.clone());
        self.send_server(ServerRequest::HaveNoParent(false)).await;
        self.send_server(ServerRequest::BranchLevel(level)).await;
        self.send_server(ServerRequest::BranchRoot(root)).await;
    }

    async fn on_parent_message(&mut self, msg: DistribMsg) {
        match msg {
            DistribMsg::Search {
                username,
                token,
                query,
            } => self.on_search_request(username, token, query).await,
            DistribMsg::BranchLevel(level) => {
                if let Some(p) = &mut self.distrib.parent {
                    p.level = level;
                    if level == 0 {
                        p.root = p.username.clone();
                    }
                }
                self.report_branch().await;
                self.emit_distrib_status();
            }
            DistribMsg::BranchRoot(root) => {
                if let Some(p) = &mut self.distrib.parent {
                    p.root = root;
                }
                self.report_branch().await;
            }
            _ => {}
        }
    }

    pub(super) async fn on_distrib_closed(&mut self, id: ConnId, username: String, reason: String) {
        let was_parent = self
            .distrib
            .parent
            .as_ref()
            .is_some_and(|p| p.username == username && p.handle.id == id);
        if was_parent {
            tracing::info!(%username, %reason, "lost distributed parent");
            self.distrib.parent = None;
            self.send_server(ServerRequest::HaveNoParent(true)).await;
            self.emit_distrib_status();
        } else if self
            .distrib
            .candidates
            .get(&username)
            .is_some_and(|c| c.handle.id == id)
        {
            self.distrib.candidates.remove(&username);
        }
    }

    /// The server embeds searches for us: we are a branch root.
    pub(super) async fn on_embedded_message(&mut self, code: u8, payload: bytes::Bytes) {
        if !self.distrib.branch_root && self.distrib.parent.is_none() {
            self.distrib.branch_root = true;
            self.emit_distrib_status();
        }
        match DistribMsg::decode_body(code, payload) {
            Ok(DistribMsg::Search {
                username,
                token,
                query,
            }) => self.on_search_request(username, token, query).await,
            Ok(_) => {}
            Err(e) => tracing::debug!(%e, "bad embedded message from server"),
        }
    }

    pub(super) async fn reset_distributed(&mut self) {
        self.distrib.parent = None;
        self.distrib.candidates.clear();
        self.distrib.branch_root = false;
        self.send_server(ServerRequest::HaveNoParent(true)).await;
        self.emit_distrib_status();
    }
}
