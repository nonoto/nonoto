//! 同期の形式を確かめる、検証用の中継。
//!
//! 製品のサーバーではなく、端末と同じプロセスのメモリの上で、暗号化したレコードを受け付けて保存し、渡す。
//! 封筒の読み取りと書き込みの署名の確かめは `nonoto-core` の関数を呼び、中継は中身を復号しない。
//!
//! 中継が保存するのは、封筒に置かれたサーバーに見せる値 (方式のID・トピックとページのID・枝・世代の番号・
//! まとめた順番の上限・世代の記録の書き込みの公開鍵と包みの宛先) と、中継が振った値 (ログの順番・まとめの受付番号) と、
//! レコードのバイト列だけである。
//! 受け取ったアカウントと時刻は、受け付けの確かめに使っても保存しない (ADR #0006)。
//! 中継は時計を持たず、時刻を受け取らない。
//!
//! 方式と形式の切り替え (ADR #0010) のため、最低限のクライアントの版より古いクライアントの書き込みを拒み、
//! 封筒の方式のIDから古い方式の暗号文が残るページを数え、0になったら古い方式を受け付けなくする。

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::ops::Bound::{Excluded, Unbounded};

pub use nonoto_core::MembershipChange;
use nonoto_core::{
    Branch, CLIENT_VERSION, Envelope, GenerationRecord, Incoming, MemberId, Membership, Outgoing,
    RecordKind, RejectReason, Role, SealedRecord, SnapshotInfo, Suite, SyncPageId, TopicId,
    WriteKeyRole,
};

pub mod measure;
pub mod network;

/// 中継がレコードを拒んだ理由。
#[derive(Debug, thiserror::Error)]
pub enum Rejection {
    /// 封筒を読めない (知らない封筒の版・方式のIDを含む) か、書き込みの署名が合わない。
    #[error("封筒を読めないか、書き込みの署名が合わない")]
    Envelope(#[source] nonoto_core::Error),

    /// 中継がそのトピックを預かっていない。
    #[error("預かっていないトピックのレコードである")]
    UnknownTopic,

    /// 中継が同じIDのトピックをすでに預かっている。
    #[error("同じIDのトピックをすでに預かっている")]
    DuplicateTopic,

    /// 送ったアカウントが、トピックのメンバーではない。
    #[error("送ったアカウントがトピックのメンバーではない")]
    UnknownAccount,

    /// 世代の記録を送ったアカウントが、編集の役割ではない。
    #[error("世代の記録を送ったアカウントが編集の役割ではない")]
    NotEditor,

    /// 同じ世代の番号の記録を、すでに受け付けている。
    #[error("同じ世代の番号の記録をすでに受け付けている: {0}")]
    DuplicateGeneration(u64),

    /// 世代の番号が、受け付けた最後の世代の次の番号ではない。
    #[error("世代の番号が、最後の世代の次の番号ではない: {0}")]
    UnexpectedGeneration(u64),

    /// 世代の記録が今のメンバー構成より前に作られた。
    #[error("世代の記録が変更前のメンバー構成で作られた")]
    StaleMembership,

    /// 世代の記録の包みの宛先に、トピックのメンバーでないメンバーがいる。
    #[error("世代の記録の包みの宛先に、トピックのメンバーでないメンバーがいる")]
    UnknownRecipient,

    /// レコードの世代の記録を、受け付けていない。
    #[error("世代の記録を受け付けていない世代のレコードである: {0}")]
    UnknownGeneration(u64),

    /// 世代を作ったあとにメンバーを外すか役割を下げたため、その世代への書き込みを受け付けない。
    #[error("メンバーを外すか役割を下げたあとの世代への書き込みである: {0}")]
    ClosedGeneration(u64),

    /// スナップショットのまとめた順番の上限が、ログの最後の順番より後にあるか、前のスナップショットより前にある。
    #[error("スナップショットのまとめた順番の上限が、ログの範囲と合わない: {0}")]
    SnapshotOutOfRange(u64),

    /// 送ったクライアントの版が、最低限のクライアントの版より古い。
    #[error("クライアントの版が最低限の版より古い: {0}")]
    ClientTooOld(u32),

    /// 受け付けなくした (廃止した) 方式のレコードである。
    #[error("受け付けなくした方式のレコードである: {0:?}")]
    RetiredSuite(Suite),

    /// 古い方式の暗号文が残るページがあり、まだ方式を受け付けなくできない。
    #[error("古い方式の暗号文が残るページがある: {0}")]
    OldCiphertextRemains(usize),
}

impl Rejection {
    /// 同期の処理が区別する、拒んだ理由に写す。
    pub fn reason(&self) -> RejectReason {
        match self {
            Self::ClosedGeneration(generation) => RejectReason::ClosedGeneration(*generation),
            Self::UnknownGeneration(generation) => RejectReason::UnknownGeneration(*generation),
            Self::DuplicateGeneration(generation) => RejectReason::DuplicateGeneration(*generation),
            Self::UnexpectedGeneration(generation) => {
                RejectReason::UnexpectedGeneration(*generation)
            }
            Self::StaleMembership => RejectReason::StaleMembership,
            Self::ClientTooOld(_) => RejectReason::ClientTooOld,
            Self::RetiredSuite(suite) => RejectReason::RetiredSuite(*suite),
            _ => RejectReason::Other,
        }
    }
}

/// 中継が受け付けたレコード。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accepted {
    /// ログのレコードに、ログの中の順番を振った。
    Log { sequence: u64 },
    /// スナップショットを受け付け、まとめた範囲のログのレコードを消した。
    Snapshot {
        removed: usize,
        /// 受け付けたスナップショットの範囲・方式・受付番号。
        info: SnapshotInfo,
    },
    /// 世代の記録を受け付けた。
    Generation { generation: u64 },
}

/// ページの1つのログ (本文か編集提案の枝)。
///
/// トピック、ページ、枝の組ごとに、別々のログとして順番を振る。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogKey {
    pub topic: TopicId,
    pub page: SyncPageId,
    pub branch: Branch,
}

/// 取得の求めに返す、ログのスナップショットと以後のレコード。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    /// 最新のスナップショット。求めた順番より後の範囲をまとめているときだけ返す。
    pub snapshot: Option<Vec<u8>>,
    /// 最新のスナップショットがあるときは、取得位置にかかわらずその情報を返す。
    pub snapshot_info: Option<SnapshotInfo>,
    /// スナップショットと求めた順番より後の、順番を振ったレコード。順番の小さい順に並べる。
    pub records: Vec<(u64, Vec<u8>)>,
}

/// 受け付けた世代の記録。
#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredGeneration {
    record: GenerationRecord,
    /// メンバーを外すか役割を下げたあとで、書き込みを拒む世代か。
    closed: bool,
}

/// 受け付けたスナップショット。
#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredSnapshot {
    info: SnapshotInfo,
    bytes: Vec<u8>,
}

/// 受け付けたログのレコード。方式は封筒のIDから読んだもので、古い方式の暗号文を数えるのに使う。
#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredRecord {
    suite: Suite,
    bytes: Vec<u8>,
}

/// ページの1つのログの、保存したレコード。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Log {
    /// 振った最後の順番。レコードを消しても戻さない。
    last_sequence: u64,
    records: BTreeMap<u64, StoredRecord>,
    snapshot: Option<StoredSnapshot>,
}

impl Log {
    /// 方式の暗号文が、レコードかスナップショットに残っているか。
    fn holds(&self, suite: Suite) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.info.suite == suite)
            || self.records.values().any(|record| record.suite == suite)
    }
}

/// トピックごとに保存した値。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Topic {
    /// テストで組んだメンバー構成。送ったアカウントの役割を確かめるのに使う。
    membership: Membership,
    membership_revision: u64,
    generations: BTreeMap<u64, StoredGeneration>,
    logs: HashMap<(SyncPageId, Branch), Log>,
    /// 受け付けなくした方式。
    retired: Vec<Suite>,
}

/// 検証用の中継。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relay {
    topics: HashMap<TopicId, Topic>,
    /// 書き込みを受け付ける、最低限のクライアントの版。
    minimum_client_version: u32,
}

impl Default for Relay {
    fn default() -> Self {
        Self {
            topics: HashMap::new(),
            minimum_client_version: CLIENT_VERSION,
        }
    }
}

impl Relay {
    /// まだどのトピックも預かっていない中継を作る。最低限のクライアントの版は、この版のクライアントの版にする。
    pub fn new() -> Self {
        Self::default()
    }

    /// 最低限のクライアントの版を返す。
    pub fn minimum_client_version(&self) -> u32 {
        self.minimum_client_version
    }

    /// 最低限のクライアントの版を決める。これより古いクライアントの書き込みを拒み、読み出しは続ける。
    pub fn set_minimum_client_version(&mut self, version: u32) {
        self.minimum_client_version = version;
    }

    /// トピックの、方式の暗号文 (ログのレコードかスナップショット) が残るページの数を、封筒の方式のIDから数える。
    ///
    /// 世代の記録は、ページの暗号文ではないため数えない。
    pub fn pages_with_suite(&self, topic: &TopicId, suite: Suite) -> usize {
        let Some(state) = self.topics.get(topic) else {
            return 0;
        };
        let mut pages: Vec<&SyncPageId> = state
            .logs
            .iter()
            .filter(|(_, log)| log.holds(suite))
            .map(|((page, _), _)| page)
            .collect();
        pages.sort_by_key(|page| page.as_bytes());
        pages.dedup();
        pages.len()
    }

    /// トピックで、方式を受け付けなくする。
    ///
    /// その方式の暗号文が残るページがあれば、[`Rejection::OldCiphertextRemains`] を返して何もしない。
    /// まとめはまとめた範囲のレコードを消すため、残るページが0になった時点で、ページの古い暗号文は中継に無い。
    pub fn retire_suite(&mut self, topic: &TopicId, suite: Suite) -> Result<(), Rejection> {
        let remaining = self.pages_with_suite(topic, suite);
        if remaining > 0 {
            return Err(Rejection::OldCiphertextRemains(remaining));
        }
        let state = self.topics.get_mut(topic).ok_or(Rejection::UnknownTopic)?;
        if !state.retired.contains(&suite) {
            state.retired.push(suite);
        }
        Ok(())
    }

    /// トピックを預かる。メンバー構成は、端末と同じものをテストの道具が渡す。
    ///
    /// 預かり済みなら [`Rejection::DuplicateTopic`] を返す。
    /// 預かっているトピックのメンバー構成は [`Relay::change_membership`] で更新する。
    pub fn add_topic(&mut self, topic: TopicId, membership: Membership) -> Result<(), Rejection> {
        let Entry::Vacant(entry) = self.topics.entry(topic) else {
            return Err(Rejection::DuplicateTopic);
        };
        entry.insert(Topic {
            membership,
            membership_revision: 0,
            generations: BTreeMap::new(),
            logs: HashMap::new(),
            retired: Vec::new(),
        });
        Ok(())
    }

    /// メンバー構成の変更を受け取る。
    ///
    /// メンバーを外すか役割を下げた変更なら、今ある世代への書き込みを、そのあとは拒む。
    pub fn change_membership(
        &mut self,
        topic: TopicId,
        membership: Membership,
        change: MembershipChange,
    ) -> Result<(), Rejection> {
        let state = self.topics.get_mut(&topic).ok_or(Rejection::UnknownTopic)?;
        state.membership = membership;
        state.membership_revision += 1;
        if change == MembershipChange::Reduced {
            for generation in state.generations.values_mut() {
                generation.closed = true;
            }
        }
        Ok(())
    }

    /// アカウントが送ったレコードを、「中継の受け付けの規則」で確かめ、受け付けたら保存する。
    ///
    /// アカウントは、送ったアカウントがトピックのメンバーかと、世代の記録を送れる役割かを確かめるのにだけ使い、保存しない。
    /// クライアントの版は、書き込みを受け付ける最低限の版との照合だけに使う。
    pub fn submit(
        &mut self,
        account: &MemberId,
        client_version: u32,
        bytes: Vec<u8>,
    ) -> Result<Accepted, Rejection> {
        self.submit_with_revision(account, bytes, None, client_version)
    }

    fn submit_with_revision(
        &mut self,
        account: &MemberId,
        bytes: Vec<u8>,
        membership_revision: Option<u64>,
        client_version: u32,
    ) -> Result<Accepted, Rejection> {
        if client_version < self.minimum_client_version {
            return Err(Rejection::ClientTooOld(client_version));
        }
        match Envelope::parse(bytes).map_err(Rejection::Envelope)? {
            Envelope::Generation(record) => {
                self.submit_generation(account, *record, membership_revision)
            }
            Envelope::Page(record) => self.submit_page(account, record),
        }
    }

    fn submit_generation(
        &mut self,
        account: &MemberId,
        record: GenerationRecord,
        membership_revision: Option<u64>,
    ) -> Result<Accepted, Rejection> {
        let header = *record.header();
        let state = self
            .topics
            .get_mut(&header.topic)
            .ok_or(Rejection::UnknownTopic)?;
        if state.retired.contains(&header.suite) {
            return Err(Rejection::RetiredSuite(header.suite));
        }
        let role = state
            .membership
            .get(account)
            .ok_or(Rejection::UnknownAccount)?
            .role;
        if role != Role::Editor {
            return Err(Rejection::NotEditor);
        }
        if membership_revision.is_some_and(|revision| revision != state.membership_revision) {
            return Err(Rejection::StaleMembership);
        }
        if state.generations.contains_key(&header.generation) {
            return Err(Rejection::DuplicateGeneration(header.generation));
        }
        let next = state
            .generations
            .last_key_value()
            .map_or(1, |(number, _)| number + 1);
        if header.generation != next {
            return Err(Rejection::UnexpectedGeneration(header.generation));
        }
        // メンバー構成の変更を知る前に作った世代の記録が、外したメンバーに新しい世代の鍵を渡さないよう、
        // 封筒にある包みの宛先が、すべて今のメンバーであることを確かめる。
        if record
            .recipients()
            .any(|recipient| state.membership.get(recipient).is_none())
        {
            return Err(Rejection::UnknownRecipient);
        }
        state.generations.insert(
            header.generation,
            StoredGeneration {
                record,
                closed: false,
            },
        );
        Ok(Accepted::Generation {
            generation: header.generation,
        })
    }

    fn submit_page(
        &mut self,
        account: &MemberId,
        record: SealedRecord,
    ) -> Result<Accepted, Rejection> {
        let header = *record.header();
        let state = self
            .topics
            .get_mut(&header.topic)
            .ok_or(Rejection::UnknownTopic)?;
        if state.membership.get(account).is_none() {
            return Err(Rejection::UnknownAccount);
        }
        if state.retired.contains(&header.suite) {
            return Err(Rejection::RetiredSuite(header.suite));
        }
        let generation = state
            .generations
            .get(&header.generation)
            .ok_or(Rejection::UnknownGeneration(header.generation))?;
        if generation.closed {
            return Err(Rejection::ClosedGeneration(header.generation));
        }
        let write_keys = generation.record.header().write_keys;
        let key = match header.write_key_role() {
            WriteKeyRole::Body => write_keys.body,
            WriteKeyRole::Proposal => write_keys.proposal,
        };
        let record = record.verify(&key).map_err(Rejection::Envelope)?;

        let log_key = (header.page, header.branch);
        match header.kind {
            RecordKind::Log => {
                let log = state.logs.entry(log_key).or_default();
                log.last_sequence += 1;
                log.records.insert(
                    log.last_sequence,
                    StoredRecord {
                        suite: header.suite,
                        bytes: record.into_bytes(),
                    },
                );
                Ok(Accepted::Log {
                    sequence: log.last_sequence,
                })
            }
            RecordKind::Snapshot { last_sequence } => {
                let (latest, previous) = state.logs.get(&log_key).map_or((0, 0), |log| {
                    (
                        log.last_sequence,
                        log.snapshot
                            .as_ref()
                            .map_or(0, |snapshot| snapshot.info.last_sequence),
                    )
                });
                if last_sequence > latest || last_sequence < previous {
                    return Err(Rejection::SnapshotOutOfRange(last_sequence));
                }
                let log = state.logs.entry(log_key).or_default();
                let before = log.records.len();
                let mut kept = log.records.split_off(&last_sequence);
                kept.remove(&last_sequence);
                let removed = before - kept.len();
                log.records = kept;
                let info = SnapshotInfo {
                    revision: log
                        .snapshot
                        .as_ref()
                        .map_or(1, |snapshot| snapshot.info.revision + 1),
                    last_sequence,
                    suite: header.suite,
                };
                log.snapshot = Some(StoredSnapshot {
                    info,
                    bytes: record.into_bytes(),
                });
                Ok(Accepted::Snapshot { removed, info })
            }
        }
    }

    /// 端末から届いたメッセージに答える。
    ///
    /// 同期の処理 ([`nonoto_core::SyncClient`]) のメッセージを、受け付け・取得の操作につなぐ。
    pub fn handle(&mut self, account: &MemberId, message: Outgoing) -> Incoming {
        match message {
            Outgoing::Submit {
                request,
                membership_revision,
                client_version,
                bytes,
            } => match self.submit_with_revision(
                account,
                bytes,
                Some(membership_revision),
                client_version,
            ) {
                Ok(Accepted::Snapshot { info, .. }) => Incoming::Compacted {
                    request,
                    snapshot: info,
                },
                Ok(Accepted::Log { sequence }) => Incoming::Accepted {
                    request,
                    sequence: Some(sequence),
                },
                Ok(Accepted::Generation { .. }) => Incoming::Accepted {
                    request,
                    sequence: None,
                },
                Err(rejection) => Incoming::Rejected {
                    request,
                    reason: rejection.reason(),
                },
            },
            Outgoing::Fetch {
                topic,
                page,
                branch,
                after,
            } => {
                let fetched = self.fetch(
                    &LogKey {
                        topic,
                        page,
                        branch,
                    },
                    after,
                );
                Incoming::Fetched {
                    page,
                    branch,
                    membership_revision: self
                        .membership_status(topic)
                        .map_or(0, |(_, revision, _)| revision),
                    minimum_client_version: self.minimum_client_version,
                    snapshot_info: fetched.snapshot_info,
                    snapshot: fetched.snapshot,
                    records: fetched.records,
                }
            }
            Outgoing::FetchGenerations { topic } => Incoming::Generations {
                records: self
                    .generation_records(&topic)
                    .map(|(number, stored)| (*number, stored.record.as_bytes().to_vec()))
                    .collect(),
            },
        }
    }

    /// テストの道具が伝える、現在のメンバー構成と更新番号、閉じた世代の上限。
    fn membership_status(&self, topic: TopicId) -> Option<(&Membership, u64, u64)> {
        let state = self.topics.get(&topic)?;
        let closed_through = state
            .generations
            .iter()
            .rev()
            .find_map(|(number, generation)| generation.closed.then_some(*number))
            .unwrap_or(0);
        Some((&state.membership, state.membership_revision, closed_through))
    }

    fn last_generation(&self, topic: TopicId) -> Option<u64> {
        self.topics
            .get(&topic)?
            .generations
            .last_key_value()
            .map(|(number, _)| *number)
    }

    /// ログの、`after` の順番より後を返す。`after` が `None` なら最初から返す。
    ///
    /// 最初からの取得か、最新のスナップショットが `after` より後までをまとめていれば、
    /// スナップショットと、その上限より後のレコードを返す。
    pub fn fetch(&self, log: &LogKey, after: Option<u64>) -> Fetched {
        let Some(stored) = self
            .topics
            .get(&log.topic)
            .and_then(|state| state.logs.get(&(log.page, log.branch)))
        else {
            return Fetched {
                snapshot: None,
                snapshot_info: None,
                records: Vec::new(),
            };
        };
        let snapshot = stored
            .snapshot
            .as_ref()
            .filter(|snapshot| after.is_none_or(|sequence| snapshot.info.last_sequence > sequence));
        let lower = snapshot
            .map(|snapshot| snapshot.info.last_sequence)
            .or(after)
            .map_or(Unbounded, Excluded);
        Fetched {
            snapshot: snapshot.map(|snapshot| snapshot.bytes.clone()),
            snapshot_info: stored.snapshot.as_ref().map(|snapshot| snapshot.info),
            records: stored
                .records
                .range((lower, Unbounded))
                .map(|(sequence, record)| (*sequence, record.bytes.clone()))
                .collect(),
        }
    }

    fn body_pages(&self, topic: &TopicId) -> Vec<SyncPageId> {
        let mut pages: Vec<_> = self
            .topics
            .get(topic)
            .into_iter()
            .flat_map(|state| state.logs.keys())
            .filter_map(|(page, branch)| (*branch == Branch::Body).then_some(*page))
            .collect();
        pages.sort_by_key(|page| *page.as_bytes());
        pages
    }

    /// トピックの、受け付けた世代の記録を世代の番号の順に返す。
    pub fn generations(&self, topic: &TopicId) -> Vec<Vec<u8>> {
        self.generation_records(topic)
            .map(|(_, stored)| stored.record.as_bytes().to_vec())
            .collect()
    }

    fn generation_records(
        &self,
        topic: &TopicId,
    ) -> impl Iterator<Item = (&u64, &StoredGeneration)> {
        self.topics
            .get(topic)
            .into_iter()
            .flat_map(|state| state.generations.iter())
    }
}

#[cfg(test)]
mod tests {
    use nonoto_core::{
        BranchId, GenerationKeys, Member, RecordHeader, Suite, UserKeys, WriteSigningKey,
    };

    use super::*;

    /// 「テストのメンバー構成」のA (編集)・B (編集)・C (提案)・D (閲覧)。
    struct Fixture {
        users: Vec<(MemberId, UserKeys)>,
        membership: Membership,
        topic: TopicId,
        page: SyncPageId,
        generations: Vec<GenerationKeys>,
    }

    const A: usize = 0;
    const B: usize = 1;
    const C: usize = 2;
    const D: usize = 3;

    impl Fixture {
        fn new() -> Self {
            let roles = [Role::Editor, Role::Editor, Role::Proposer, Role::Viewer];
            let users: Vec<(MemberId, UserKeys)> = roles
                .iter()
                .map(|_| {
                    (
                        MemberId::generate().expect("メンバーのIDを作れなかった"),
                        UserKeys::generate(Suite::CURRENT).expect("利用者の鍵を作れなかった"),
                    )
                })
                .collect();
            let membership = membership(&users, &roles);
            Self {
                users,
                membership,
                topic: TopicId::generate().expect("トピックのIDを作れなかった"),
                page: SyncPageId::generate().expect("ページのIDを作れなかった"),
                generations: (0..3)
                    .map(|_| GenerationKeys::generate().expect("世代の鍵を作れなかった"))
                    .collect(),
            }
        }

        fn id(&self, index: usize) -> MemberId {
            self.users[index].0
        }

        fn keys(&self, generation: u64) -> &GenerationKeys {
            &self.generations[usize::try_from(generation - 1).expect("世代の番号が大きすぎる")]
        }

        /// トピックを預け、Aが作った世代1を受け付けた中継。
        fn relay(&self) -> Relay {
            self.relay_with(self.generation(A, 1))
        }

        /// トピックを預け、渡した世代1の記録を受け付けた中継。
        fn relay_with(&self, first: Vec<u8>) -> Relay {
            let mut relay = Relay::new();
            relay
                .add_topic(self.topic, self.membership.clone())
                .expect("トピックを預けられなかった");
            relay
                .submit(&self.id(A), CLIENT_VERSION, first)
                .expect("世代1の記録を拒んだ");
            relay
        }

        fn generation(&self, creator: usize, generation: u64) -> Vec<u8> {
            self.generation_for(creator, generation, &self.membership)
        }

        fn generation_for(
            &self,
            creator: usize,
            generation: u64,
            membership: &Membership,
        ) -> Vec<u8> {
            GenerationRecord::create(
                Suite::CURRENT,
                self.topic,
                generation,
                self.keys(generation),
                self.id(creator),
                &self.users[creator].1,
                membership,
            )
            .expect("世代の記録を作れなかった")
            .into_bytes()
        }

        fn record(
            &self,
            kind: RecordKind,
            branch: Branch,
            generation: u64,
            write_key: &WriteSigningKey,
        ) -> Vec<u8> {
            SealedRecord::seal(
                RecordHeader {
                    kind,
                    suite: Suite::CURRENT,
                    topic: self.topic,
                    page: self.page,
                    branch,
                    generation,
                },
                b"payload",
                self.keys(generation).topic_key(),
                write_key,
            )
            .expect("レコードを封じられなかった")
            .into_bytes()
        }

        fn body(&self, generation: u64) -> Vec<u8> {
            self.record(
                RecordKind::Log,
                Branch::Body,
                generation,
                self.keys(generation).body_key(),
            )
        }

        fn body_snapshot(&self, last_sequence: u64) -> Vec<u8> {
            self.record(
                RecordKind::Snapshot { last_sequence },
                Branch::Body,
                1,
                self.keys(1).body_key(),
            )
        }

        fn body_log(&self) -> LogKey {
            LogKey {
                topic: self.topic,
                page: self.page,
                branch: Branch::Body,
            }
        }
    }

    fn membership(users: &[(MemberId, UserKeys)], roles: &[Role]) -> Membership {
        Membership::new(
            users
                .iter()
                .zip(roles)
                .map(|((id, keys), role)| Member {
                    id: *id,
                    role: *role,
                    keys: keys.public_keys().clone(),
                })
                .collect(),
        )
        .expect("メンバー構成を作れなかった")
    }

    fn proposal() -> Branch {
        Branch::Proposal(BranchId::generate().expect("枝のIDを作れなかった"))
    }

    #[test]
    fn 同じトピックを再登録しても受け付け済みの履歴を保つ() {
        let fixture = Fixture::new();
        let mut relay = fixture.relay();
        for record in [fixture.body(1), fixture.body_snapshot(1), fixture.body(1)] {
            relay
                .submit(&fixture.id(A), CLIENT_VERSION, record)
                .expect("再登録前のレコードを拒んだ");
        }
        let before = relay.clone();

        assert!(
            matches!(
                relay.add_topic(fixture.topic, fixture.membership.clone()),
                Err(Rejection::DuplicateTopic)
            ),
            "同じトピックをもう一度登録できた",
        );
        assert_eq!(relay, before, "再登録を拒んだときに履歴が変わった");
        assert_eq!(
            relay
                .submit(&fixture.id(A), CLIENT_VERSION, fixture.body(1))
                .expect("再登録を拒んだ後のレコードを拒んだ"),
            Accepted::Log { sequence: 3 },
            "再登録を拒んだ後の順番が以前の続きにならなかった",
        );
    }

    #[test]
    fn 本文のログのレコードに順番を振って保存し_取得の求めに返す() {
        let fixture = Fixture::new();
        let mut relay = fixture.relay();
        let first = fixture.body(1);
        let second = fixture.body(1);

        for (bytes, expected) in [(first.clone(), 1), (second.clone(), 2)] {
            assert_eq!(
                relay
                    .submit(&fixture.id(D), CLIENT_VERSION, bytes)
                    .expect("本文のログのレコードを拒んだ"),
                Accepted::Log { sequence: expected },
                "振った順番が受け付けた順と異なる",
            );
        }

        assert_eq!(
            relay.fetch(&fixture.body_log(), None),
            Fetched {
                snapshot: None,
                snapshot_info: None,
                records: vec![(1, first), (2, second.clone())],
            },
            "最初からの取得の結果が、受け付けたレコードと異なる",
        );
        assert_eq!(
            relay.fetch(&fixture.body_log(), Some(1)).records,
            vec![(2, second)],
            "順番1より後の取得の結果が、2つ目のレコードではない",
        );
        assert_eq!(
            relay.fetch(&fixture.body_log(), Some(u64::MAX)),
            Fetched {
                snapshot: None,
                snapshot_info: None,
                records: Vec::new(),
            },
            "最大の順番より後の取得でレコードを返した",
        );
    }

    #[test]
    fn 提案の書き込みの鍵で署名した本文への書き込みを拒む() {
        let fixture = Fixture::new();
        let mut relay = fixture.relay();
        let bytes = fixture.record(
            RecordKind::Log,
            Branch::Body,
            1,
            fixture.keys(1).proposal_key(),
        );

        assert!(
            matches!(
                relay.submit(&fixture.id(C), CLIENT_VERSION, bytes),
                Err(Rejection::Envelope(
                    nonoto_core::Error::InvalidWriteSignature
                ))
            ),
            "提案の書き込みの鍵で署名した本文への書き込みを受け付けた",
        );
        assert!(
            relay.fetch(&fixture.body_log(), None).records.is_empty(),
            "拒んだレコードを保存した",
        );
    }

    #[test]
    fn 編集提案の枝には提案の書き込みの鍵の署名だけを受け付ける() {
        let fixture = Fixture::new();
        let mut relay = fixture.relay();
        let branch = proposal();

        let proposal_key =
            fixture.record(RecordKind::Log, branch, 1, fixture.keys(1).proposal_key());
        assert_eq!(
            relay
                .submit(&fixture.id(C), CLIENT_VERSION, proposal_key)
                .expect("提案の書き込みの鍵の枝への書き込みを拒んだ"),
            Accepted::Log { sequence: 1 },
            "枝のログの順番が1から始まらない",
        );
        let body_key = fixture.record(RecordKind::Log, branch, 1, fixture.keys(1).body_key());
        assert!(
            matches!(
                relay.submit(&fixture.id(A), CLIENT_VERSION, body_key),
                Err(Rejection::Envelope(
                    nonoto_core::Error::InvalidWriteSignature
                ))
            ),
            "本文の書き込みの鍵で署名した枝への書き込みを受け付けた",
        );
    }

    #[test]
    fn 知らない封筒の版と方式のレコードを拒む() {
        let fixture = Fixture::new();
        let mut relay = fixture.relay();
        let mut version = fixture.body(1);
        version[4..6].copy_from_slice(&2u16.to_be_bytes());
        let mut suite = fixture.body(1);
        suite[7..9].copy_from_slice(&0xffffu16.to_be_bytes());

        assert!(
            matches!(
                relay.submit(&fixture.id(A), CLIENT_VERSION, version),
                Err(Rejection::Envelope(
                    nonoto_core::Error::UnsupportedEnvelopeVersion(2)
                ))
            ),
            "知らない封筒の版のレコードを受け付けた",
        );
        assert!(
            matches!(
                relay.submit(&fixture.id(A), CLIENT_VERSION, suite),
                Err(Rejection::Envelope(nonoto_core::Error::UnsupportedSuite(
                    0xffff
                )))
            ),
            "知らない方式のレコードを受け付けた",
        );
    }

    #[test]
    fn 世代の記録は編集の役割のアカウントからだけ受け付ける() {
        let fixture = Fixture::new();
        let mut relay = fixture.relay();
        // 世代2の記録はAが作ったものだが、提案と閲覧の役割のアカウントが送っても受け付けない。
        for index in [C, D] {
            assert!(
                matches!(
                    relay.submit(&fixture.id(index), CLIENT_VERSION, fixture.generation(A, 2)),
                    Err(Rejection::NotEditor)
                ),
                "編集の役割でないアカウントが送った世代の記録を受け付けた",
            );
        }
        assert_eq!(
            relay
                .submit(&fixture.id(B), CLIENT_VERSION, fixture.generation(A, 2))
                .expect("編集の役割のアカウントが送った世代の記録を拒んだ"),
            Accepted::Generation { generation: 2 },
            "受け付けた世代の番号が異なる",
        );
    }

    #[test]
    fn 同じ世代の番号の2つ目の記録を拒む() {
        let fixture = Fixture::new();
        let mut relay = fixture.relay();
        let accepted = fixture.generation(A, 2);
        relay
            .submit(&fixture.id(A), CLIENT_VERSION, accepted.clone())
            .expect("世代2の記録を拒んだ");
        // Bが同時に作った世代2の記録は、あとから届いたため受け付けない。
        let late = fixture.generation(B, 2);

        assert!(
            matches!(
                relay.submit(&fixture.id(B), CLIENT_VERSION, late),
                Err(Rejection::DuplicateGeneration(2))
            ),
            "同じ世代の番号の2つ目の記録を受け付けた",
        );
        assert_eq!(
            relay.generations(&fixture.topic).last(),
            Some(&accepted),
            "世代2の記録が、最初に届いた記録ではない",
        );
    }

    #[test]
    fn 次の番号でない世代の記録を拒む() {
        let fixture = Fixture::new();
        let mut relay = fixture.relay();

        assert!(
            matches!(
                relay.submit(&fixture.id(A), CLIENT_VERSION, fixture.generation(A, 3)),
                Err(Rejection::UnexpectedGeneration(3))
            ),
            "番号を飛ばした世代の記録を受け付けた",
        );
    }

    #[test]
    fn メンバーを外したあとの古い世代への書き込みを拒み_新しい世代への書き込みを受け付ける() {
        let fixture = Fixture::new();
        let mut relay = fixture.relay();
        relay
            .submit(&fixture.id(A), CLIENT_VERSION, fixture.body(1))
            .expect("外す前の世代1への書き込みを拒んだ");

        // Dを外す。
        let remaining = membership(
            &fixture.users[..D],
            &[Role::Editor, Role::Editor, Role::Proposer],
        );
        relay
            .change_membership(fixture.topic, remaining.clone(), MembershipChange::Reduced)
            .expect("メンバー構成の変更を受け取れなかった");
        assert!(
            matches!(
                relay.submit(&fixture.id(A), CLIENT_VERSION, fixture.body(1)),
                Err(Rejection::ClosedGeneration(1))
            ),
            "メンバーを外したあとの古い世代への書き込みを受け付けた",
        );

        let new_generation = fixture.generation_for(A, 2, &remaining);
        relay
            .submit(&fixture.id(A), CLIENT_VERSION, new_generation)
            .expect("外したあとの新しい世代の記録を拒んだ");
        let stored = relay
            .generations(&fixture.topic)
            .pop()
            .expect("受け付けた世代の記録が無い");
        let stored = GenerationRecord::parse(stored).expect("世代の記録を読めなかった");
        assert_eq!(
            stored.recipients().copied().collect::<Vec<_>>(),
            remaining.iter().map(|member| member.id).collect::<Vec<_>>(),
            "メンバーを外したあとの世代の宛先が、残ったメンバーと異なる",
        );
        assert_eq!(
            relay
                .submit(&fixture.id(A), CLIENT_VERSION, fixture.body(2))
                .expect("新しい世代への書き込みを拒んだ"),
            Accepted::Log { sequence: 2 },
            "新しい世代への書き込みの順番が、古い世代の続きではない",
        );
        assert!(
            matches!(
                relay.submit(&fixture.id(D), CLIENT_VERSION, fixture.body(2)),
                Err(Rejection::UnknownAccount)
            ),
            "外したメンバーのアカウントの書き込みを受け付けた",
        );
    }

    #[test]
    fn 外したメンバーを宛先に含む新しい世代の記録を拒む() {
        let fixture = Fixture::new();
        let mut relay = fixture.relay();
        // 外す変更を知る前に、Aが外す前のメンバー構成 (Dを含む) で作った世代2の記録。
        let stale = fixture.generation(A, 2);
        let remaining = membership(
            &fixture.users[..D],
            &[Role::Editor, Role::Editor, Role::Proposer],
        );
        relay
            .change_membership(fixture.topic, remaining.clone(), MembershipChange::Reduced)
            .expect("メンバー構成の変更を受け取れなかった");
        let before = relay.clone();

        assert!(
            matches!(
                relay.submit(&fixture.id(A), CLIENT_VERSION, stale),
                Err(Rejection::UnknownRecipient)
            ),
            "外したメンバーを宛先に含む世代の記録を受け付けた",
        );
        assert_eq!(relay, before, "拒んだ世代の記録で中継の状態が変わった");
        assert_eq!(
            relay
                .submit(
                    &fixture.id(A),
                    CLIENT_VERSION,
                    fixture.generation_for(A, 2, &remaining)
                )
                .expect("残ったメンバーだけに包んだ世代の記録を拒んだ"),
            Accepted::Generation { generation: 2 },
            "作り直した世代の記録の番号が異なる",
        );
    }

    #[test]
    fn 預かっていないトピックのレコードとメンバー構成の変更を拒む() {
        let fixture = Fixture::new();
        let mut relay = Relay::new();
        let before = relay.clone();

        assert!(
            matches!(
                relay.submit(&fixture.id(A), CLIENT_VERSION, fixture.generation(A, 1)),
                Err(Rejection::UnknownTopic)
            ),
            "預かっていないトピックの世代の記録を受け付けた",
        );
        assert!(
            matches!(
                relay.submit(&fixture.id(A), CLIENT_VERSION, fixture.body(1)),
                Err(Rejection::UnknownTopic)
            ),
            "預かっていないトピックのログのレコードを受け付けた",
        );
        assert!(
            matches!(
                relay.change_membership(
                    fixture.topic,
                    fixture.membership.clone(),
                    MembershipChange::Extended
                ),
                Err(Rejection::UnknownTopic)
            ),
            "預かっていないトピックのメンバー構成の変更を受け取った",
        );
        assert_eq!(
            relay, before,
            "預かっていないトピックのレコードと変更で中継の状態が変わった"
        );
    }

    #[test]
    fn 役割を上げたら新しい役割で受け付け_役割を下げたら古い世代への書き込みを拒む() {
        let fixture = Fixture::new();
        let mut relay = fixture.relay();
        let raised = membership(
            &fixture.users,
            &[Role::Editor, Role::Editor, Role::Proposer, Role::Editor],
        );

        // Dの役割を閲覧から編集に上げる。
        relay
            .change_membership(fixture.topic, raised.clone(), MembershipChange::Extended)
            .expect("メンバー構成の変更を受け取れなかった");
        relay
            .submit(&fixture.id(A), CLIENT_VERSION, fixture.body(1))
            .expect("役割を上げたあとの世代1への書き込みを拒んだ");
        let generation = fixture.generation_for(D, 2, &raised);
        assert_eq!(
            relay
                .submit(&fixture.id(D), CLIENT_VERSION, generation)
                .expect("編集に上がったメンバーが送った世代の記録を拒んだ"),
            Accepted::Generation { generation: 2 },
            "編集に上がったメンバーが送った世代の番号が異なる",
        );

        // Bの役割を提案に下げる。
        let queued = fixture.generation_for(B, 3, &raised);
        let lowered = [Role::Editor, Role::Proposer, Role::Proposer, Role::Editor];
        relay
            .change_membership(
                fixture.topic,
                membership(&fixture.users, &lowered),
                MembershipChange::Reduced,
            )
            .expect("メンバー構成の変更を受け取れなかった");
        for generation in [1, 2] {
            assert!(
                matches!(
                    relay.submit(&fixture.id(A), CLIENT_VERSION, fixture.body(generation)),
                    Err(Rejection::ClosedGeneration(closed)) if closed == generation
                ),
                "役割を下げた後の世代{generation}への書き込みを受け付けた",
            );
        }
        assert!(
            matches!(
                relay.submit(&fixture.id(B), CLIENT_VERSION, queued),
                Err(Rejection::NotEditor)
            ),
            "役割を下げたメンバーのアカウントから世代の記録を受け付けた",
        );
    }

    #[test]
    fn 世代の記録の無い世代への書き込みを拒む() {
        let fixture = Fixture::new();
        let mut relay = fixture.relay();

        assert!(
            matches!(
                relay.submit(&fixture.id(A), CLIENT_VERSION, fixture.body(2)),
                Err(Rejection::UnknownGeneration(2))
            ),
            "世代の記録の無い世代への書き込みを受け付けた",
        );
    }

    #[test]
    fn スナップショットを受け付けたらまとめた範囲を消し_取得の求めにスナップショットと以後を返す() {
        let fixture = Fixture::new();
        let mut relay = fixture.relay();
        let records: Vec<Vec<u8>> = (0..3).map(|_| fixture.body(1)).collect();
        for bytes in &records {
            relay
                .submit(&fixture.id(A), CLIENT_VERSION, bytes.clone())
                .expect("本文のログのレコードを拒んだ");
        }
        let snapshot = fixture.body_snapshot(2);
        let info = SnapshotInfo {
            revision: 1,
            last_sequence: 2,
            suite: Suite::CURRENT,
        };

        assert_eq!(
            relay
                .submit(&fixture.id(A), CLIENT_VERSION, snapshot.clone())
                .expect("スナップショットを拒んだ"),
            Accepted::Snapshot { removed: 2, info },
            "まとめた範囲のレコードを消した数が異なる",
        );
        assert_eq!(
            relay.fetch(&fixture.body_log(), None),
            Fetched {
                snapshot: Some(snapshot),
                snapshot_info: Some(info),
                records: vec![(3, records[2].clone())],
            },
            "最初からの取得で、スナップショットと以後のレコードを返さなかった",
        );
        assert_eq!(
            relay.fetch(&fixture.body_log(), Some(2)),
            Fetched {
                snapshot: None,
                snapshot_info: Some(info),
                records: vec![(3, records[2].clone())],
            },
            "まとめた範囲より後からの取得で、スナップショットを返した",
        );
    }

    #[test]
    fn 最初から取得すると上限が0のスナップショットも返す() {
        let fixture = Fixture::new();
        let mut relay = fixture.relay();
        let snapshot = fixture.body_snapshot(0);
        let info = SnapshotInfo {
            revision: 1,
            last_sequence: 0,
            suite: Suite::CURRENT,
        };
        assert_eq!(
            relay
                .submit(&fixture.id(A), CLIENT_VERSION, snapshot.clone())
                .expect("上限0のスナップショットを拒んだ"),
            Accepted::Snapshot { removed: 0, info },
            "更新がまだ無いログからレコードを削除した",
        );
        let update = fixture.body(1);
        relay
            .submit(&fixture.id(A), CLIENT_VERSION, update.clone())
            .expect("スナップショットの後のレコードを拒んだ");
        let log = fixture.body_log();

        assert_eq!(
            relay.fetch(&log, None),
            Fetched {
                snapshot: Some(snapshot),
                snapshot_info: Some(info),
                records: vec![(1, update.clone())],
            },
            "初回取得で上限0のスナップショットと以後のログを返さなかった",
        );
        assert_eq!(
            relay.fetch(&log, Some(0)),
            Fetched {
                snapshot: None,
                snapshot_info: Some(info),
                records: vec![(1, update)],
            },
            "順番0の後からの取得で上限0のスナップショットを返した",
        );
    }

    #[test]
    fn 提案の書き込みの鍵で署名したスナップショットと_範囲の合わないスナップショットを拒む() {
        let fixture = Fixture::new();
        let mut relay = fixture.relay();
        relay
            .submit(&fixture.id(A), CLIENT_VERSION, fixture.body(1))
            .expect("本文のログのレコードを拒んだ");

        let proposal_key = fixture.record(
            RecordKind::Snapshot { last_sequence: 1 },
            proposal(),
            1,
            fixture.keys(1).proposal_key(),
        );
        assert!(
            matches!(
                relay.submit(&fixture.id(C), CLIENT_VERSION, proposal_key),
                Err(Rejection::Envelope(
                    nonoto_core::Error::InvalidWriteSignature
                ))
            ),
            "提案の書き込みの鍵で署名したスナップショットを受け付けた",
        );
        let beyond = fixture.record(
            RecordKind::Snapshot { last_sequence: 5 },
            Branch::Body,
            1,
            fixture.keys(1).body_key(),
        );
        assert!(
            matches!(
                relay.submit(&fixture.id(A), CLIENT_VERSION, beyond),
                Err(Rejection::SnapshotOutOfRange(5))
            ),
            "ログの最後の順番より後までをまとめたスナップショットを受け付けた",
        );
    }

    #[test]
    fn 範囲外のスナップショットを拒んでも中継の状態は変わらない() {
        let fixture = Fixture::new();
        let mut relay = fixture.relay();
        let before = relay.clone();
        assert!(
            matches!(
                relay.submit(&fixture.id(A), CLIENT_VERSION, fixture.body_snapshot(1)),
                Err(Rejection::SnapshotOutOfRange(1))
            ),
            "まだ無いログの順番1をまとめるスナップショットを受け付けた",
        );
        assert_eq!(relay, before, "スナップショットを拒んだのにログを作った");

        for record in [fixture.body(1), fixture.body_snapshot(1), fixture.body(1)] {
            relay
                .submit(&fixture.id(A), CLIENT_VERSION, record)
                .expect("範囲内のレコードを拒んだ");
        }
        let before = relay.clone();
        assert!(
            matches!(
                relay.submit(&fixture.id(A), CLIENT_VERSION, fixture.body_snapshot(0)),
                Err(Rejection::SnapshotOutOfRange(0))
            ),
            "保存済みのスナップショットより古い上限を受け付けた",
        );
        assert_eq!(relay, before, "古い上限を拒んだのに保存済みの履歴を変えた");
    }

    #[test]
    fn 送ったアカウントを保存しない() {
        // 同じレコードを、別のアカウントから同じ順に送った2つの中継は、同じ値を保存する。
        let fixture = Fixture::new();
        let first = fixture.generation(A, 1);
        let mut from_a = fixture.relay_with(first.clone());
        let mut from_b = fixture.relay_with(first);
        let generation = fixture.generation(A, 2);
        let records = [
            fixture.body(1),
            fixture.record(
                RecordKind::Log,
                proposal(),
                1,
                fixture.keys(1).proposal_key(),
            ),
        ];

        from_a
            .submit(&fixture.id(A), CLIENT_VERSION, generation.clone())
            .expect("世代の記録を拒んだ");
        from_b
            .submit(&fixture.id(B), CLIENT_VERSION, generation)
            .expect("世代の記録を拒んだ");
        for bytes in records {
            from_a
                .submit(&fixture.id(A), CLIENT_VERSION, bytes.clone())
                .expect("レコードを拒んだ");
            from_b
                .submit(&fixture.id(D), CLIENT_VERSION, bytes)
                .expect("レコードを拒んだ");
        }

        assert_eq!(
            from_a, from_b,
            "送ったアカウントによって、中継の保存した値が変わった"
        );
    }

    #[test]
    fn 保存するレコードは封筒に置いた値と順番とバイト列だけで_中身を復号しない() {
        let fixture = Fixture::new();
        let mut relay = fixture.relay();
        let bytes = fixture.body(1);
        relay
            .submit(&fixture.id(A), CLIENT_VERSION, bytes.clone())
            .expect("本文のログのレコードを拒んだ");

        let state = relay
            .topics
            .get(&fixture.topic)
            .expect("トピックを預かっていない");
        let (key, log) = state.logs.iter().next().expect("ログが無い");
        assert_eq!(
            *key,
            (fixture.page, Branch::Body),
            "ログの見分けが、封筒のページと枝ではない"
        );
        assert_eq!(
            log.records.get(&1).map(|record| &record.bytes),
            Some(&bytes),
            "保存したバイト列が、届いたレコードと異なる"
        );
        assert_eq!(
            state.generations.keys().copied().collect::<Vec<_>>(),
            vec![1],
            "保存した世代の番号が、受け付けた世代と異なる",
        );
    }

    #[test]
    fn 直接の書き込みでも最低限のクライアントの版を確かめる() {
        let fixture = Fixture::new();
        let mut relay = fixture.relay();
        relay.set_minimum_client_version(CLIENT_VERSION + 1);
        let update = fixture.body(1);
        assert!(matches!(
            relay.submit(&fixture.id(A), CLIENT_VERSION, update.clone()),
            Err(Rejection::ClientTooOld(CLIENT_VERSION))
        ));
        assert_eq!(relay.fetch(&fixture.body_log(), None).records, Vec::new());
        assert_eq!(
            relay
                .submit(&fixture.id(A), CLIENT_VERSION + 1, update)
                .expect("書き込める版を拒んだ"),
            Accepted::Log { sequence: 1 }
        );
    }

    #[test]
    fn 古い方式の暗号文を数え_残るあいだは方式を受け付けなくしない() {
        let fixture = Fixture::new();
        let mut relay = fixture.relay();
        relay
            .submit(&fixture.id(A), CLIENT_VERSION, fixture.body(1))
            .expect("本文のログのレコードを拒んだ");
        let proposal = Branch::Proposal(BranchId::generate().expect("枝のIDを作れなかった"));
        relay
            .submit(
                &fixture.id(C),
                CLIENT_VERSION,
                fixture.record(RecordKind::Log, proposal, 1, fixture.keys(1).proposal_key()),
            )
            .expect("編集提案の枝のレコードを拒んだ");
        assert_eq!(
            relay.pages_with_suite(&fixture.topic, Suite::CURRENT),
            1,
            "本文と枝のログを、同じページとして数えていない"
        );
        assert_eq!(relay.pages_with_suite(&fixture.topic, Suite::TestSecond), 0);
        assert!(
            matches!(
                relay.retire_suite(&fixture.topic, Suite::CURRENT),
                Err(Rejection::OldCiphertextRemains(1))
            ),
            "古い暗号文が残るのに、方式を受け付けなくした",
        );
        relay
            .submit(&fixture.id(A), CLIENT_VERSION, fixture.body(1))
            .expect("受け付けなくしていない方式のレコードを拒んだ");
    }
}
