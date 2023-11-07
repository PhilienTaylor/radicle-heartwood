use std::{fmt, io, path::Path};

use radicle::crypto::Signature;
use sqlite as sql;
use thiserror::Error;

use crate::node::NodeId;
use crate::prelude::{Filter, Timestamp};
use crate::service::message::{
    Announcement, AnnouncementMessage, InventoryAnnouncement, NodeAnnouncement, RefsAnnouncement,
};
use crate::wire;
use crate::wire::Decode;

#[derive(Error, Debug)]
pub enum Error {
    /// I/O error.
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
    /// An Internal error.
    #[error("internal error: {0}")]
    Internal(#[from] sql::Error),
}

/// Keeps track of the latest received gossip messages for each node.
/// Grows linearly with the number of nodes on the network.
pub struct GossipStore {
    db: sql::Connection,
}

impl fmt::Debug for GossipStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GossipStore").finish()
    }
}

impl GossipStore {
    /// Open a gossip store at the given path. Creates a new store if it doesn't exist.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        let db = sql::Connection::open_with_flags(
            path,
            sqlite::OpenFlags::new().set_read_write().set_full_mutex(),
        )?;

        Ok(Self { db })
    }

    /// Prune announcements older than the cutoff time.
    pub fn prune(&mut self, cutoff: Timestamp) -> Result<usize, Error> {
        let mut stmt = self
            .db
            .prepare("DELETE FROM `announcements` WHERE timestamp < ?1")?;

        stmt.bind((1, cutoff.try_into().unwrap_or(i64::MAX)))?;
        stmt.next()?;

        Ok(self.db.change_count())
    }

    /// Process an announcement for the given node.
    /// Returns `true` if the timestamp was updated or the announcement wasn't there before.
    pub fn announced(&mut self, nid: &NodeId, ann: &Announcement) -> Result<bool, Error> {
        let mut stmt = self.db.prepare(
            "INSERT INTO `announcements` (node, repo, type, message, signature, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT DO UPDATE
             SET message = ?4, signature = ?5, timestamp = ?6
             WHERE timestamp < ?6",
        )?;
        stmt.bind((1, nid))?;

        match &ann.message {
            AnnouncementMessage::Node(msg) => {
                stmt.bind((2, sql::Value::String(String::new())))?;
                stmt.bind((3, &GossipType::Node))?;
                stmt.bind((4, msg))?;
            }
            AnnouncementMessage::Refs(msg) => {
                stmt.bind((2, &msg.rid))?;
                stmt.bind((3, &GossipType::Refs))?;
                stmt.bind((4, msg))?;
            }
            AnnouncementMessage::Inventory(msg) => {
                stmt.bind((2, sql::Value::String(String::new())))?;
                stmt.bind((3, &GossipType::Inventory))?;
                stmt.bind((4, msg))?;
            }
        }
        stmt.bind((5, &ann.signature))?;
        stmt.bind((6, ann.message.timestamp().try_into().unwrap_or(i64::MAX)))?;
        stmt.next()?;

        Ok(self.db.change_count() > 0)
    }

    /// Get all the latest gossip messages of all nodes, filtered by inventory filter and
    /// announcement timestamps.
    ///
    /// # Panics
    ///
    /// Panics if `from` > `to`.
    ///
    pub fn filtered<'a>(
        &'a self,
        filter: &'a Filter,
        from: Timestamp,
        to: Timestamp,
    ) -> Result<impl Iterator<Item = Result<Announcement, Error>> + 'a, Error> {
        let mut stmt = self.db.prepare(
            "SELECT node, type, message, signature, timestamp
             FROM announcements
             WHERE timestamp >= ?1 and timestamp < ?2
             ORDER BY timestamp",
        )?;
        assert!(from <= to);

        stmt.bind((1, i64::try_from(from).unwrap_or(i64::MAX)))?;
        stmt.bind((2, i64::try_from(to).unwrap_or(i64::MAX)))?;

        Ok(stmt
            .into_iter()
            .map(|row| {
                let row = row?;
                let node = row.read::<NodeId, _>("node");
                let gt = row.read::<GossipType, _>("type");
                let message = match gt {
                    GossipType::Refs => {
                        let ann = row.read::<RefsAnnouncement, _>("message");
                        AnnouncementMessage::Refs(ann)
                    }
                    GossipType::Inventory => {
                        let ann = row.read::<InventoryAnnouncement, _>("message");
                        AnnouncementMessage::Inventory(ann)
                    }
                    GossipType::Node => {
                        let ann = row.read::<NodeAnnouncement, _>("message");
                        AnnouncementMessage::Node(ann)
                    }
                };
                let signature = row.read::<Signature, _>("signature");
                let timestamp = row.read::<i64, _>("timestamp");

                debug_assert_eq!(timestamp, message.timestamp() as i64);

                Ok(Announcement {
                    node,
                    message,
                    signature,
                })
            })
            .filter(|ann| match ann {
                Ok(a) => a.matches(filter),
                Err(_) => true,
            }))
    }
}

impl TryFrom<&sql::Value> for NodeAnnouncement {
    type Error = sql::Error;

    fn try_from(value: &sql::Value) -> Result<Self, Self::Error> {
        match value {
            sql::Value::Binary(bytes) => {
                let mut reader = io::Cursor::new(bytes);
                NodeAnnouncement::decode(&mut reader).map_err(wire::Error::into)
            }
            _ => Err(sql::Error {
                code: None,
                message: Some("sql: invalid type for node announcement".to_owned()),
            }),
        }
    }
}

impl sql::BindableWithIndex for &NodeAnnouncement {
    fn bind<I: sql::ParameterIndex>(self, stmt: &mut sql::Statement<'_>, i: I) -> sql::Result<()> {
        wire::serialize(self).bind(stmt, i)
    }
}

impl TryFrom<&sql::Value> for RefsAnnouncement {
    type Error = sql::Error;

    fn try_from(value: &sql::Value) -> Result<Self, Self::Error> {
        match value {
            sql::Value::Binary(bytes) => {
                let mut reader = io::Cursor::new(bytes);
                RefsAnnouncement::decode(&mut reader).map_err(wire::Error::into)
            }
            _ => Err(sql::Error {
                code: None,
                message: Some("sql: invalid type for refs announcement".to_owned()),
            }),
        }
    }
}

impl sql::BindableWithIndex for &RefsAnnouncement {
    fn bind<I: sql::ParameterIndex>(self, stmt: &mut sql::Statement<'_>, i: I) -> sql::Result<()> {
        wire::serialize(self).bind(stmt, i)
    }
}

impl TryFrom<&sql::Value> for InventoryAnnouncement {
    type Error = sql::Error;

    fn try_from(value: &sql::Value) -> Result<Self, Self::Error> {
        match value {
            sql::Value::Binary(bytes) => {
                let mut reader = io::Cursor::new(bytes);
                InventoryAnnouncement::decode(&mut reader).map_err(wire::Error::into)
            }
            _ => Err(sql::Error {
                code: None,
                message: Some("sql: invalid type for inventory announcement".to_owned()),
            }),
        }
    }
}

impl sql::BindableWithIndex for &InventoryAnnouncement {
    fn bind<I: sql::ParameterIndex>(self, stmt: &mut sql::Statement<'_>, i: I) -> sql::Result<()> {
        wire::serialize(self).bind(stmt, i)
    }
}

impl From<wire::Error> for sql::Error {
    fn from(other: wire::Error) -> Self {
        sql::Error {
            code: None,
            message: Some(other.to_string()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GossipType {
    Refs,
    Node,
    Inventory,
}

impl fmt::Display for GossipType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refs => write!(f, "refs"),
            Self::Node => write!(f, "node"),
            Self::Inventory => write!(f, "inventory"),
        }
    }
}

impl sql::BindableWithIndex for &GossipType {
    fn bind<I: sql::ParameterIndex>(self, stmt: &mut sql::Statement<'_>, i: I) -> sql::Result<()> {
        self.to_string().as_str().bind(stmt, i)
    }
}

impl TryFrom<&sql::Value> for GossipType {
    type Error = sql::Error;

    fn try_from(value: &sql::Value) -> Result<Self, Self::Error> {
        match value {
            sql::Value::String(s) => match s.as_str() {
                "refs" => Ok(Self::Refs),
                "node" => Ok(Self::Node),
                "inventory" => Ok(Self::Inventory),
                other => Err(sql::Error {
                    code: None,
                    message: Some(format!("unknown gossip type '{other}'")),
                }),
            },
            _ => Err(sql::Error {
                code: None,
                message: Some("sql: invalid type for gossip type".to_owned()),
            }),
        }
    }
}
