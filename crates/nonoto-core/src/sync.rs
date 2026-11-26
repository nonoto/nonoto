//! 同期の処理。通信を持たず、受け取ったメッセージを渡すと、送るメッセージと文書に起きた変更を返す。
//!
//! 端末は、トピックごとに [`SyncClient`] を持つ。同期の状態はメモリの上だけに持ち、保存しない。
//!
//! - ページの本文のログごとに、取得した最後の順番を持つ
//! - 手元の更新は、送ったが受け付けを知らないものも含めて、受け付けを知るまで持つ。
//!   つなぎ直したら、続きを取得してから送り直す。同じレコードが重ねて届いても、文書は変わらない
//! - 送る直前に、メンバー構成の変更を確かめる。メンバーを外すか役割を下げる変更を知ったら古い世代では送らず、
//!   新しい世代がまだ無く、自分が編集の役割なら、作って配ってから送る (ADR #0009)
//! - 世代の記録が拒まれたら (ほかの端末が先に作った)、受け付けられた世代を取得し、その鍵でまだ受け付けられていない更新を暗号化し直す
//! - 編集の役割の端末は、本文のログを取得した最後の順番までまとめたスナップショットを送る。
//!   まとめは、今の世代の鍵と、今書ける方式で暗号化する (ADR #0008・ADR #0010)
//! - 書く方式は、中継の最低限のクライアントの版から決め、廃止した方式では書かない ([`SuitePolicy`])。
//!   知らない封筒の版・方式・中身の版のレコードを受け取ったページは、適用を止め、まとめも送信もしない

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fmt;
use std::ops::Range;

use crate::author::Writer;
use crate::compaction::{VersionRetention, check_snapshot};
use crate::crypto::{Suite, UserVerifyingKey};
use crate::document::{Document, Replacement, Update, Version};
use crate::error::Error;
use crate::generation::{GenerationKeys, GenerationRecord, RoleKeys, WriteKeys};
use crate::member::{MemberId, Membership, Role, UserKeys};
use crate::receive::{CheckedEntry, CheckedVersion, GenerationView, check_log_entry};
use crate::record::{Branch, RecordHeader, RecordKind, SealedRecord, SyncPageId, TopicId};
use crate::version::ContentHash;

/// この版のクライアントの版。中継は、最低限の版より古いクライアントの書き込みを拒む (ADR #0010)。
pub const CLIENT_VERSION: u32 = 1;

/// 端末が読み書きできる方式と、端末のクライアントの版。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuitePolicy {
    /// この端末のクライアントの版。
    pub client_version: u32,
    /// 読める方式。ほかの方式のレコードを受け取ったページは、適用を止める。
    pub readable: Vec<Suite>,
    /// 書いてよい方式と、その方式で書き始めてよい中継の最低限のクライアントの版。新しい方式を後ろに並べる。
    ///
    /// 最低限の版は、その方式を読めるクライアントの版にする。
    /// 並べない方式 (廃止した方式) では、中継の最低限の版がどうであっても書かない。
    pub writable: Vec<(Suite, u32)>,
}

impl SuitePolicy {
    /// この版のクライアントの方式。
    pub fn current() -> Self {
        Self {
            client_version: CLIENT_VERSION,
            readable: Suite::ALL.to_vec(),
            writable: vec![(Suite::CURRENT, CLIENT_VERSION)],
        }
    }

    /// 中継の最低限のクライアントの版で書ける方式のうち、最も新しいものを返す。
    ///
    /// この端末が最低限の版より古いか、書ける方式が無ければ `None` を返す。
    pub fn write_suite(&self, minimum_client_version: u32) -> Option<Suite> {
        if self.client_version < minimum_client_version {
            return None;
        }
        self.writable
            .iter()
            .rev()
            .find(|(suite, since)| {
                *since <= minimum_client_version && self.readable.contains(suite)
            })
            .map(|(suite, _)| *suite)
    }
}

impl Default for SuitePolicy {
    fn default() -> Self {
        Self::current()
    }
}

/// 送ったレコードと、その受け付けの結果を結ぶ番号。端末の中で振る。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestId(u64);

/// 端末から中継へ送るメッセージ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outgoing {
    /// レコード (ページのログのレコードか世代の記録) を送る。
    Submit {
        request: RequestId,
        /// 端末がテストの道具の通知 ([`SyncClient::change_membership`]) で知った、メンバー構成の更新番号。
        /// 中継は世代の記録を受け付けるときに今の番号と照らし、違えば変更前の構成で作った記録として拒む。
        membership_revision: u64,
        /// 送った端末のクライアントの版。中継は最低限の版より古ければ拒む。
        client_version: u32,
        bytes: Vec<u8>,
    },
    /// ログの、`after` の順番より後を求める。`after` が `None` なら最初から求める。
    Fetch {
        topic: TopicId,
        page: SyncPageId,
        branch: Branch,
        after: Option<u64>,
    },
    /// トピックの世代の記録を求める。
    FetchGenerations { topic: TopicId },
}

/// 中継がレコードを拒んだ理由のうち、同期の処理が区別するもの。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// メンバーを外すか役割を下げたあとの世代への書き込みだった。
    ClosedGeneration(u64),
    /// 中継が世代の記録を受け付けていない世代への書き込みだった。
    UnknownGeneration(u64),
    /// 同じ世代の番号の記録を、中継がすでに受け付けていた。
    DuplicateGeneration(u64),
    /// 世代の番号が、中継が受け付けた最後の世代の次ではなかった。
    UnexpectedGeneration(u64),
    /// 世代の記録が、変更前のメンバー構成で作られた。
    StaleMembership,
    /// 送った端末のクライアントの版が、中継の最低限の版より古い。読み出しは続けられる。
    ClientTooOld,
    /// 中継がこの方式の受け付けを終了した。取得し直すと、書ける方式を選び直せる。
    RetiredSuite(Suite),
    /// そのほかの理由。送り直しても受け付けられない。
    Other,
}

/// 中継の本文のログに保存されたスナップショットの情報。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotInfo {
    /// 同じ順番までをまとめ直したときも増える、このログでのまとめの受付番号。
    pub revision: u64,
    /// まとめに含むログの順番の上限。
    pub last_sequence: u64,
    /// 保存したスナップショットの封筒の方式。
    pub suite: Suite,
}

/// 中継から端末へ届くメッセージ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming {
    /// 送ったレコードを受け付けた。ログのレコードなら、振った順番を持つ。
    Accepted {
        request: RequestId,
        sequence: Option<u64>,
    },
    /// まとめを受け付けた。中継に保存したスナップショットの情報を持つ。
    Compacted {
        request: RequestId,
        snapshot: SnapshotInfo,
    },
    /// 送ったレコードを拒んだ。
    Rejected {
        request: RequestId,
        reason: RejectReason,
    },
    /// 取得の結果。
    Fetched {
        page: SyncPageId,
        branch: Branch,
        /// 答えたときに中継が持っていた、メンバー構成の更新番号。
        /// この番号までの著者の公開鍵がそろうと、未知の著者のレコードを読み飛ばせる。
        membership_revision: u64,
        /// 答えたときの、中継の最低限のクライアントの版。端末は書く方式をこれで決める。
        minimum_client_version: u32,
        /// 最新のスナップショットの情報。本文のバイト列を返さない取得結果にも入れる。
        snapshot_info: Option<SnapshotInfo>,
        snapshot: Option<Vec<u8>>,
        records: Vec<(u64, Vec<u8>)>,
    },
    /// トピックの世代の記録を、世代の番号の順に並べたもの。
    Generations { records: Vec<(u64, Vec<u8>)> },
}

/// 文書に起きた変更。
#[derive(Clone, PartialEq, Eq)]
pub struct PageChange {
    pub page: SyncPageId,
    /// 本文の置き換えの列。タイトルの変更は [`Document::title`] で読む。
    pub replacements: Vec<Replacement>,
}

impl fmt::Debug for PageChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PageChange")
            .field("page", &self.page)
            .field("replacement_count", &self.replacements.len())
            .finish()
    }
}

/// 同期の処理の1回の結果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Step {
    /// 中継へ送るメッセージ。並べた順に送る。
    pub outgoing: Vec<Outgoing>,
    /// 文書に起きた変更。
    pub changes: Vec<PageChange>,
    /// 中継が恒久的に拒み、送信を止めたページ。編集は送信待ちに残る ([`SyncClient::is_blocked`])。
    pub blocked: Vec<SyncPageId>,
    /// 知らない版か方式のレコードを受け取り、適用を止めたページ ([`SyncClient::is_stalled`])。
    pub stalled: Vec<SyncPageId>,
}

/// メンバー構成の変更の種類。テストの道具が端末に知らせる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MembershipChange {
    /// メンバーを足したか、役割を上げた。
    Extended,
    /// メンバーを外したか、役割を下げた。今ある世代では、もう送らない。
    Reduced,
}

/// 開いた世代の鍵。
#[derive(Debug, Clone)]
struct Generation {
    write_keys: WriteKeys,
    role_keys: RoleKeys,
}

/// 作って中継に送った、まだ受け付けを知らない世代。
#[derive(Debug)]
struct Creating {
    request: RequestId,
    number: u64,
    generation: Generation,
}

/// 手元で書いた、本文のログに送るもの。
#[derive(Debug)]
enum Entry {
    Update(Update),
    /// 版を付けた時点の版の値と中身のハッシュ。送る時点で署名して封じる。
    Version {
        version: Version,
        content_hash: ContentHash,
    },
}

/// 手元で書いた、受け付けを知らない更新か版の記録。
#[derive(Debug)]
struct Pending {
    entry: Entry,
    /// 送ったときの番号と世代。まだ送っていないか、送り直すなら `None`。
    sent: Option<(RequestId, u64)>,
}

/// ページの本文のログの、同期の状態。
#[derive(Debug)]
struct PageState {
    document: Document,
    writer: Writer,
    /// 取得した最後の順番。まだ取得していなければ `None`。
    last_sequence: Option<u64>,
    /// 手元で編集し、受付を知ったレコードの最後の順番。
    accepted_through: Option<u64>,
    pending: VecDeque<Pending>,
    /// つないでから、続きを取得し終えたか。取得し終えるまで、手元の更新を送らない。
    caught_up: bool,
    /// 取得結果で知った最後の順番。古い応答で追いついたと判断しないために残す。
    fetched_through: Option<u64>,
    /// 世代か著者の鍵を待っている順番と、取得する世代の番号。
    read_wait: Option<ReadWait>,
    /// 送信か適用を止めた理由。止めていなければ `None`。
    halt: Option<Halt>,
    /// 本文のログにある (まとめたものを含む) 版の記録。
    versions: Vec<CheckedVersion>,
    /// 中継に残っている本文のログのレコードの方式と順番。
    stored_suites: Vec<(u64, Suite)>,
    snapshot_info: Option<SnapshotInfo>,
    compaction: Option<Compaction>,
    /// 中継の受付を待っているまとめの要求番号。
    compacting: Option<RequestId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReadWait {
    sequence: u64,
    /// 世代の記録を取得する必要があれば、その番号。
    generation: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Compaction {
    Manual,
    Rewrite,
}

/// ページの送信か適用を止めた理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Halt {
    /// 恒久拒否された手元の更新があり、依存する更新を送れない。届いたレコードは当てる。
    Blocked,
    /// 知らない版か方式のレコードを受け取り、適用もまとめも送信も止めている。
    Stalled,
}

impl PageState {
    fn next_update(&self) -> Option<&Pending> {
        if !self.caught_up || self.halt.is_some() {
            return None;
        }
        // 後の更新は先の更新に依存するため、受付を1件ずつ確かめる。
        self.pending
            .front()
            .filter(|pending| pending.sent.is_none())
    }

    fn can_compact(&self) -> bool {
        self.halt.is_none()
            && self.caught_up
            && self.last_sequence.is_some()
            && self.last_sequence >= self.accepted_through
            && self.pending.is_empty()
            && self.compacting.is_none()
    }

    fn contains_other_suite(&self, suite: Suite) -> bool {
        self.snapshot_info
            .is_some_and(|snapshot| snapshot.suite != suite)
            || self
                .stored_suites
                .iter()
                .any(|(_, stored)| *stored != suite)
    }

    fn remember_snapshot(&mut self, info: SnapshotInfo) {
        if self
            .snapshot_info
            .is_none_or(|previous| info.revision > previous.revision)
        {
            self.stored_suites
                .retain(|(sequence, _)| *sequence > info.last_sequence);
            self.snapshot_info = Some(info);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SendWait {
    Ready,
    /// 受け付けられた世代が見つからなければ、次の取得まで作り直さない。
    NextPoll,
    /// 中継が新しいメンバー構成を知っており、この端末への通知を待つ。
    Membership,
}

/// トピック1つの、端末の同期の処理。
#[derive(Debug)]
pub struct SyncClient {
    topic: TopicId,
    me: MemberId,
    keys: UserKeys,
    policy: SuitePolicy,
    /// 中継から知った、最低限のクライアントの版の最大。
    minimum_client_version: u32,
    retired_suites: HashSet<Suite>,
    membership: Membership,
    membership_revision: u64,
    /// 著者の署名を確かめるための、これまでに知ったメンバーの署名の公開鍵。外したメンバーの鍵も残す。
    known_keys: HashMap<MemberId, UserVerifyingKey>,
    /// その時点までのすべてのメンバーの署名の公開鍵がそろっている、メンバー構成の更新番号。
    author_keys_through: Option<u64>,
    /// テストの道具から受け取った、世代の受付時の認証済みのメンバー構成。
    generation_memberships: HashMap<u64, Membership>,
    generations: BTreeMap<u64, Generation>,
    unreadable_generations: HashSet<u64>,
    /// メンバーを外すか役割を下げたときに中継が閉じた、最後の世代の番号。
    closed_through: u64,
    creating: Option<Creating>,
    fetching_generations: bool,
    send_wait: SendWait,
    connected: bool,
    next_request: u64,
    pages: HashMap<SyncPageId, PageState>,
}

impl SyncClient {
    /// メンバー構成の `me` のメンバーとして、トピックの同期の処理を作る。まだつながっていない。
    pub fn new(
        topic: TopicId,
        me: MemberId,
        keys: UserKeys,
        membership: Membership,
    ) -> Result<Self, Error> {
        if membership.get(&me).is_none() {
            return Err(Error::UnknownMember);
        }
        let mut client = Self {
            topic,
            me,
            keys,
            policy: SuitePolicy::current(),
            minimum_client_version: 0,
            retired_suites: HashSet::new(),
            membership: Membership::default(),
            membership_revision: 0,
            known_keys: HashMap::new(),
            author_keys_through: None,
            generation_memberships: HashMap::new(),
            generations: BTreeMap::new(),
            unreadable_generations: HashSet::new(),
            closed_through: 0,
            creating: None,
            fetching_generations: false,
            send_wait: SendWait::Ready,
            connected: false,
            next_request: 0,
            pages: HashMap::new(),
        };
        client.learn(membership);
        Ok(client)
    }

    /// 読み書きできる方式とクライアントの版を差し替える。方式の切り替えを確かめるテストで、古い端末と新しい端末を作るのに使う。
    #[must_use]
    pub fn with_policy(mut self, policy: SuitePolicy) -> Self {
        self.policy = policy;
        self
    }

    /// 中継の最低限のクライアントの版で、いま書く方式を返す。書ける方式が無ければ `None`。
    pub fn write_suite(&self) -> Option<Suite> {
        self.policy
            .write_suite(self.minimum_client_version)
            .filter(|suite| !self.retired_suites.contains(suite))
    }

    /// ページを開く。空の文書から始め、つないだら最初から取得する。
    ///
    /// 初めて開くときに新しい乱数で書き始める ([`Writer::start`])。
    /// すでに開いているページは、その文書と送信待ちの更新を保つ。
    pub fn open_page(&mut self, page: SyncPageId) -> Result<Step, Error> {
        if self.pages.contains_key(&page) {
            return Ok(Step::default());
        }
        let member = self.membership.get(&self.me).ok_or(Error::UnknownMember)?;
        let writer = Writer::start(member, &self.keys)?;
        let mut document = Document::new("")?;
        document.set_writer(writer.id())?;
        self.pages.insert(
            page,
            PageState {
                document,
                writer,
                last_sequence: None,
                accepted_through: None,
                pending: VecDeque::new(),
                caught_up: false,
                fetched_through: None,
                read_wait: None,
                halt: None,
                versions: Vec::new(),
                stored_suites: Vec::new(),
                snapshot_info: None,
                compaction: None,
                compacting: None,
            },
        );
        let mut step = Step::default();
        if self.connected {
            step.outgoing.push(self.fetch(page));
        }
        Ok(step)
    }

    /// この端末が同期しているトピックのID。
    pub fn topic(&self) -> TopicId {
        self.topic
    }

    /// 開いているページの、手元の編集も含む現在の文書を返す。
    pub fn document(&self, page: &SyncPageId) -> Option<&Document> {
        self.pages.get(page).map(|state| &state.document)
    }

    /// 送ったが受け付けを知らない更新と、まだ送っていない更新の数を返す。
    pub fn pending(&self, page: &SyncPageId) -> usize {
        self.pages.get(page).map_or(0, |state| state.pending.len())
    }

    /// ページの送信が、中継の恒久的な拒否で止まっているかを返す。
    ///
    /// 止まったページでも手元で編集でき、編集は送信待ちに残るが、中継には送らない。
    pub fn is_blocked(&self, page: &SyncPageId) -> bool {
        self.pages
            .get(page)
            .is_some_and(|state| state.halt == Some(Halt::Blocked))
    }

    /// 知らない版か方式のレコードを受け取り、ページへの適用を止めているかを返す。
    ///
    /// 止めたページには、届いたレコードを当てず、まとめも手元の編集の送信もしない。
    pub fn is_stalled(&self, page: &SyncPageId) -> bool {
        self.pages
            .get(page)
            .is_some_and(|state| state.halt == Some(Halt::Stalled))
    }

    /// 本文のログにある版の記録を、ログの順に返す。まとめたスナップショットから受け取った版を含む。
    pub fn versions(&self, page: &SyncPageId) -> &[CheckedVersion] {
        self.pages
            .get(page)
            .map_or(&[], |state| state.versions.as_slice())
    }

    /// 中継に、いま書く方式と違う方式の暗号文が、このページの本文のログに残っているかを返す。
    pub fn needs_rewrite(&self, page: &SyncPageId) -> bool {
        self.write_suite().is_some_and(|suite| {
            self.pages
                .get(page)
                .is_some_and(|state| state.contains_other_suite(suite))
        })
    }

    /// 送る先の世代の番号を返す。まだ送れる世代が無ければ `None`。
    pub fn current_generation(&self) -> Option<u64> {
        self.current().map(|(number, _)| number)
    }

    /// 手元で本文を編集し、送れるなら送る。
    pub fn edit(
        &mut self,
        page: SyncPageId,
        range: Range<usize>,
        text: &str,
    ) -> Result<Step, Error> {
        let state = self.pages.get_mut(&page).ok_or(Error::UnknownPage)?;
        if !is_editor(&self.membership, self.me) {
            return Err(Error::NotEditor);
        }
        if let Some(update) = state.document.apply_local(range, text)? {
            state.pending.push_back(Pending {
                entry: Entry::Update(update),
                sent: None,
            });
        }
        self.flush()
    }

    /// 手元の文書の今の時点に版を付け、版の記録を本文のログに送る。
    pub fn create_version(&mut self, page: SyncPageId) -> Result<Step, Error> {
        if !is_editor(&self.membership, self.me) {
            return Err(Error::NotEditor);
        }
        let state = self.pages.get_mut(&page).ok_or(Error::UnknownPage)?;
        let version = state.document.version();
        let content_hash = ContentHash::of(&state.document.content_at(&version)?);
        state.pending.push_back(Pending {
            entry: Entry::Version {
                version,
                content_hash,
            },
            sent: None,
        });
        self.flush()
    }

    /// 本文のログを、取得した最後の順番までまとめたスナップショットにして送る。
    ///
    /// 手元で先に受け付けられた編集まで取得し終え、送信待ちが無いときにまとめる。
    /// 送れる世代が無ければ世代を用意し、そのあとにまとめを送る。
    /// 取得が済んでいなければ [`Error::CompactionNotReady`]、適用を止めたページは
    /// [`Error::PageStalled`]、書ける方式が無ければ [`Error::NoWritableSuite`] を返す。
    pub fn compact(&mut self, page: SyncPageId) -> Result<Step, Error> {
        if !is_editor(&self.membership, self.me) {
            return Err(Error::NotEditor);
        }
        let can_write = self.write_suite().is_some();
        let state = self.pages.get_mut(&page).ok_or(Error::UnknownPage)?;
        if state.halt == Some(Halt::Stalled) {
            return Err(Error::PageStalled);
        }
        if !self.connected || !state.can_compact() {
            return Err(Error::CompactionNotReady);
        }
        if !can_write {
            return Err(Error::NoWritableSuite);
        }
        state.compaction = Some(Compaction::Manual);
        self.flush()
    }

    /// 同期に登録したページの取得を進め、古い方式の暗号文が残るページをまとめ直す。
    ///
    /// 取得や世代の用意が済むと、受け付けたメッセージへの応答としてまとめを送る。
    /// 適用を止めたページは飛ばす。
    pub fn rewrite_old_pages(&mut self) -> Result<Step, Error> {
        if !is_editor(&self.membership, self.me) {
            return Err(Error::NotEditor);
        }
        let mut pages = Vec::new();
        for (page, state) in &mut self.pages {
            if state.halt.is_none() {
                state.compaction.get_or_insert(Compaction::Rewrite);
                if state.caught_up && state.compacting.is_none() {
                    state.caught_up = false;
                    pages.push(*page);
                }
            }
        }
        pages.sort_by_key(|page| *page.as_bytes());
        let mut step = Step::default();
        if self.connected {
            step.outgoing
                .extend(pages.into_iter().map(|page| self.fetch(page)));
        }
        Ok(step)
    }

    /// 中継につなぐ。世代の記録と、開いたページの続きを求め、取得し終えてから手元の更新を送る。
    pub fn connect(&mut self) -> Step {
        self.connected = true;
        self.fetching_generations = true;
        if self.send_wait == SendWait::NextPoll {
            self.send_wait = SendWait::Ready;
        }
        let mut step = Step::default();
        step.outgoing
            .push(Outgoing::FetchGenerations { topic: self.topic });
        for state in self.pages.values_mut() {
            state.caught_up = false;
        }
        step.outgoing
            .extend(self.pages.keys().map(|page| self.fetch(*page)));
        step
    }

    /// 中継との接続が切れた。受け付けを知らない更新と世代は、つなぎ直したあとに確かめ直す。
    pub fn disconnect(&mut self) {
        self.connected = false;
        self.fetching_generations = false;
        self.creating = None;
        for state in self.pages.values_mut() {
            state.caught_up = false;
            state.compacting = None;
            if let Some(pending) = state.pending.front_mut() {
                pending.sent = None;
            }
        }
    }

    /// 開いたページの続きを求める。ほかの端末の更新を取りに行くときに呼ぶ。
    pub fn poll(&mut self) -> Step {
        if self.send_wait == SendWait::NextPoll {
            self.send_wait = SendWait::Ready;
        }
        let mut step = Step::default();
        if self.connected {
            let pages: Vec<SyncPageId> = self.pages.keys().copied().collect();
            for page in pages {
                step.outgoing.push(self.fetch(page));
            }
        }
        step
    }

    /// テストの道具から、メンバー構成とその更新番号、中継が閉じた世代の上限を受け取る。
    ///
    /// 変更前の世代の作成を取り消し、閉じた世代を送信先から外す。
    /// 取得結果を受け取ったあとに送信することで、通知と暗号化を分ける。
    pub fn change_membership(
        &mut self,
        membership: Membership,
        revision: u64,
        closed_through: u64,
    ) -> Step {
        if revision <= self.membership_revision {
            return Step::default();
        }
        self.membership_revision = revision;
        self.learn(membership);
        self.send_wait = SendWait::Ready;
        self.closed_through = self.closed_through.max(closed_through);
        self.creating = None;
        for state in self.pages.values_mut() {
            state.caught_up = false;
            state.compacting = None;
            if let Some(pending) = state.pending.front_mut() {
                pending.sent = None;
            }
        }
        self.poll()
    }

    /// テストの道具から、過去のメンバーを含む認証済みの署名の公開鍵を受け取る。
    ///
    /// 初期構成から `through_revision` までの公開鍵がそろっていれば、その番号を渡す。
    /// 受け取った鍵は次にログを取得したときに使う。
    pub fn remember_author_keys(
        &mut self,
        keys: impl IntoIterator<Item = (MemberId, UserVerifyingKey)>,
        through_revision: Option<u64>,
    ) {
        for (id, key) in keys {
            self.known_keys.entry(id).or_insert(key);
        }
        self.author_keys_through = self.author_keys_through.max(through_revision);
    }

    /// テストの道具から、世代の受付時に認証済みだったメンバー構成を受け取る。
    ///
    /// 現在の役割と切り離し、過去の世代と署名の確認に使う。
    pub fn remember_generation_membership(&mut self, number: u64, membership: Membership) {
        for member in membership.iter() {
            self.known_keys
                .entry(member.id)
                .or_insert(member.keys.signing);
        }
        self.generation_memberships
            .entry(number)
            .or_insert(membership);
    }

    /// 中継から届いたメッセージを当てる。
    pub fn receive(&mut self, message: Incoming) -> Result<Step, Error> {
        match message {
            Incoming::Accepted { request, sequence } => self.accepted(request, sequence),
            Incoming::Compacted { request, snapshot } => self.compacted(request, snapshot),
            Incoming::Rejected { request, reason } => self.rejected(request, reason),
            Incoming::Generations { records } => self.generations_fetched(records),
            Incoming::Fetched {
                page,
                branch,
                membership_revision,
                minimum_client_version,
                snapshot_info,
                snapshot,
                records,
            } => {
                self.minimum_client_version =
                    self.minimum_client_version.max(minimum_client_version);
                self.fetched(
                    page,
                    branch,
                    membership_revision,
                    snapshot_info,
                    snapshot,
                    records,
                )
            }
        }
    }

    fn learn(&mut self, membership: Membership) {
        for member in membership.iter() {
            self.known_keys.insert(member.id, member.keys.signing);
        }
        self.membership = membership;
    }

    fn fetch(&self, page: SyncPageId) -> Outgoing {
        Outgoing::Fetch {
            topic: self.topic,
            page,
            branch: Branch::Body,
            after: self.pages.get(&page).and_then(|state| state.last_sequence),
        }
    }

    fn request(&mut self) -> RequestId {
        self.next_request += 1;
        RequestId(self.next_request)
    }

    /// 送れる世代 (閉じていない最後の世代) を返す。
    fn current(&self) -> Option<(u64, &Generation)> {
        self.generations
            .iter()
            .next_back()
            .filter(|(number, generation)| {
                **number > self.closed_through && generation.role_keys.body_key().is_some()
            })
            .map(|(number, generation)| (*number, generation))
    }

    /// 送れるものを送る。送れる世代が無ければ、編集の役割なら新しい世代を作り、そうでなければ世代の記録を求める。
    fn flush(&mut self) -> Result<Step, Error> {
        let mut step = Step::default();
        if !self.connected
            || self.creating.is_some()
            || self.fetching_generations
            || self.send_wait == SendWait::Membership
            || !is_editor(&self.membership, self.me)
        {
            return Ok(step);
        }
        let Some(suite) = self.write_suite() else {
            // 廃止した方式では書かず、書ける方式が決まるまで送信待ちに残す。
            return Ok(step);
        };
        let compactions = self.compaction_jobs(suite);
        if compactions.is_empty()
            && !self
                .pages
                .values()
                .any(|state| state.next_update().is_some())
        {
            return Ok(step);
        }
        let Some((number, generation)) = self.current() else {
            step.outgoing.extend(self.start_generation(suite)?);
            return Ok(step);
        };
        let topic_key = generation.role_keys.topic_key().clone();
        let body_key = generation
            .role_keys
            .body_key()
            .ok_or(Error::NotEditor)?
            .clone();
        let mut requests = Vec::new();
        for (page, state) in &self.pages {
            let Some(pending) = state.next_update() else {
                continue;
            };
            let header = RecordHeader {
                kind: RecordKind::Log,
                suite,
                topic: self.topic,
                page: *page,
                branch: Branch::Body,
                generation: number,
            };
            let record = match &pending.entry {
                Entry::Update(update) => state
                    .writer
                    .seal_update(header, update, &topic_key, &body_key)?,
                Entry::Version {
                    version,
                    content_hash,
                } => state.writer.seal_version_entry(
                    header,
                    version.clone(),
                    content_hash.clone(),
                    &topic_key,
                    &body_key,
                )?,
            };
            let bytes = record.into_bytes();
            requests.push((*page, bytes));
        }
        let compactions = compactions
            .into_iter()
            .map(|page| {
                self.seal_compaction(page, number, generation, suite)
                    .map(|bytes| (page, bytes))
            })
            .collect::<Result<Vec<_>, Error>>()?;
        for (page, bytes) in requests {
            let request = self.request();
            if let Some(pending) = self
                .pages
                .get_mut(&page)
                .and_then(|state| state.pending.front_mut())
            {
                pending.sent = Some((request, number));
            }
            step.outgoing.push(Outgoing::Submit {
                request,
                membership_revision: self.membership_revision,
                client_version: self.policy.client_version,
                bytes,
            });
        }
        for (page, bytes) in compactions {
            let request = self.request();
            if let Some(state) = self.pages.get_mut(&page) {
                state.compacting = Some(request);
            }
            step.outgoing.push(Outgoing::Submit {
                request,
                membership_revision: self.membership_revision,
                client_version: self.policy.client_version,
                bytes,
            });
        }
        Ok(step)
    }

    fn compaction_jobs(&mut self, suite: Suite) -> Vec<SyncPageId> {
        let mut compactions = Vec::new();
        for (page, state) in &mut self.pages {
            if state.compaction.is_none() || !state.can_compact() {
                continue;
            }
            if state.compaction == Some(Compaction::Rewrite) && !state.contains_other_suite(suite) {
                state.compaction = None;
            } else {
                compactions.push(*page);
            }
        }
        compactions.sort_by_key(|page| *page.as_bytes());
        compactions
    }

    fn seal_compaction(
        &self,
        page: SyncPageId,
        number: u64,
        generation: &Generation,
        suite: Suite,
    ) -> Result<Vec<u8>, Error> {
        let state = self.pages.get(&page).ok_or(Error::UnknownPage)?;
        let sequence = state.last_sequence.ok_or(Error::CompactionNotReady)?;
        let header = RecordHeader {
            kind: RecordKind::Snapshot {
                last_sequence: sequence,
            },
            suite,
            topic: self.topic,
            page,
            branch: Branch::Body,
            generation: number,
        };
        // 中身がハッシュと合わない版は、どの端末でも取り出せないため、まとめに入れない。
        let versions: Vec<_> = state
            .versions
            .iter()
            .filter(|version| version.content(&state.document).is_ok())
            .cloned()
            .collect();
        Ok(state
            .writer
            .seal_snapshot(
                header,
                &state.document,
                &versions,
                VersionRetention::History,
                generation.role_keys.topic_key(),
                generation.role_keys.body_key().ok_or(Error::NotEditor)?,
            )?
            .into_bytes())
    }

    /// 書き手が送れる世代を持っていなければ、新しい世代を作る。
    fn start_generation(&mut self, suite: Suite) -> Result<Option<Outgoing>, Error> {
        if self.send_wait != SendWait::Ready {
            return Ok(None);
        }
        let number = self
            .generations
            .keys()
            .next_back()
            .copied()
            .unwrap_or(0)
            .max(self.closed_through)
            + 1;
        let keys = GenerationKeys::generate()?;
        let record = GenerationRecord::create(
            suite,
            self.topic,
            number,
            &keys,
            self.me,
            &self.keys,
            &self.membership,
        )?;
        let role_keys = record.open(self.me, &self.keys, &self.membership)?;
        let request = self.request();
        self.creating = Some(Creating {
            request,
            number,
            generation: Generation {
                write_keys: keys.write_keys(),
                role_keys,
            },
        });
        Ok(Some(Outgoing::Submit {
            request,
            membership_revision: self.membership_revision,
            client_version: self.policy.client_version,
            bytes: record.into_bytes(),
        }))
    }

    fn accepted(&mut self, request: RequestId, sequence: Option<u64>) -> Result<Step, Error> {
        if let Some(creating) = self
            .creating
            .take_if(|creating| creating.request == request)
        {
            self.generations
                .insert(creating.number, creating.generation);
            return self.flush();
        }
        for (page, state) in &mut self.pages {
            if state
                .pending
                .front()
                .is_some_and(|pending| pending.sent.is_some_and(|(sent, _)| sent == request))
            {
                state.accepted_through = state.accepted_through.max(sequence);
                state.pending.pop_front();
                let refetch = (state.compaction.is_some()
                    && state.pending.is_empty()
                    && state.last_sequence < state.accepted_through)
                    .then_some(*page);
                if refetch.is_some() {
                    state.caught_up = false;
                }
                let mut step = self.flush()?;
                if let Some(page) = refetch {
                    step.outgoing.push(self.fetch(page));
                }
                return Ok(step);
            }
        }
        Ok(Step::default())
    }

    fn compacted(&mut self, request: RequestId, snapshot: SnapshotInfo) -> Result<Step, Error> {
        for state in self.pages.values_mut() {
            if state.compacting == Some(request) {
                state.compacting = None;
                state.compaction = None;
                state.remember_snapshot(snapshot);
                return self.flush();
            }
        }
        Ok(Step::default())
    }

    fn rejected(&mut self, request: RequestId, reason: RejectReason) -> Result<Step, Error> {
        if self
            .creating
            .take_if(|creating| creating.request == request)
            .is_some()
        {
            match reason {
                RejectReason::StaleMembership => {
                    self.send_wait = SendWait::Membership;
                    return Ok(Step::default());
                }
                RejectReason::RetiredSuite(suite) => {
                    self.retired_suites.insert(suite);
                    return Ok(self.poll());
                }
                _ => {}
            }
            self.send_wait = SendWait::NextPoll;
            // 受け付けられた世代を取得する。見つからなければ次の poll まで作り直さない。
            self.fetching_generations = true;
            return Ok(Step {
                outgoing: vec![Outgoing::FetchGenerations { topic: self.topic }],
                ..Step::default()
            });
        }
        for (page, state) in &mut self.pages {
            if state.compacting != Some(request) {
                continue;
            }
            state.compacting = None;
            match reason {
                RejectReason::RetiredSuite(suite) => {
                    self.retired_suites.insert(suite);
                }
                RejectReason::ClosedGeneration(generation)
                | RejectReason::UnknownGeneration(generation) => {
                    self.closed_through = self.closed_through.max(generation);
                }
                _ => {
                    state.compaction = None;
                    return Ok(Step::default());
                }
            }
            state.caught_up = false;
            let page = *page;
            return Ok(Step {
                outgoing: vec![self.fetch(page)],
                ..Step::default()
            });
        }
        let mut step = Step::default();
        let mut refetch = Vec::new();
        for (page, state) in &mut self.pages {
            let Some(pending) = state
                .pending
                .front_mut()
                .filter(|pending| pending.sent.is_some_and(|(sent, _)| sent == request))
            else {
                continue;
            };
            pending.sent = None;
            match reason {
                RejectReason::ClosedGeneration(generation)
                | RejectReason::UnknownGeneration(generation) => {
                    self.closed_through = self.closed_through.max(generation);
                }
                RejectReason::StaleMembership => {
                    // 新しい構成を受け取るまで、この更新を送り直さない。
                    state.caught_up = false;
                    self.send_wait = SendWait::Membership;
                }
                RejectReason::RetiredSuite(suite) => {
                    self.retired_suites.insert(suite);
                    state.caught_up = false;
                    refetch.push(*page);
                }
                RejectReason::DuplicateGeneration(_)
                | RejectReason::UnexpectedGeneration(_)
                | RejectReason::ClientTooOld
                | RejectReason::Other => {
                    if state.halt.is_none() {
                        state.halt = Some(Halt::Blocked);
                    }
                    step.blocked.push(*page);
                }
            }
        }
        step.outgoing
            .extend(refetch.into_iter().map(|page| self.fetch(page)));
        let flushed = self.flush()?;
        step.outgoing.extend(flushed.outgoing);
        Ok(step)
    }

    fn generations_fetched(&mut self, records: Vec<(u64, Vec<u8>)>) -> Result<Step, Error> {
        self.fetching_generations = false;
        for (number, bytes) in records {
            if self.generations.contains_key(&number) {
                continue;
            }
            let record = match GenerationRecord::parse(bytes) {
                Ok(record) => record,
                Err(err) if stops_page(&err) => {
                    self.unreadable_generations.insert(number);
                    continue;
                }
                Err(err) => return Err(err),
            };
            if record.header().topic != self.topic || record.header().generation != number {
                continue;
            }
            if !self.policy.readable.contains(&record.header().suite) {
                self.unreadable_generations.insert(number);
                continue;
            }
            let Some(membership) = self.generation_memberships.get(&number) else {
                continue;
            };
            match record.open(self.me, &self.keys, membership) {
                Ok(role_keys) => {
                    self.unreadable_generations.remove(&number);
                    self.generations.insert(
                        number,
                        Generation {
                            write_keys: record.header().write_keys,
                            role_keys,
                        },
                    );
                }
                Err(err) if stops_page(&err) => {
                    self.unreadable_generations.insert(number);
                }
                Err(_) => {}
            }
        }
        let mut step = Step::default();
        if self.send_wait == SendWait::NextPoll && self.current().is_some() {
            self.send_wait = SendWait::Ready;
        }
        step.outgoing
            .extend(self.pages.iter().filter_map(|(page, state)| {
                let number = state.read_wait?.generation?;
                (self.generations.contains_key(&number)
                    || self.unreadable_generations.contains(&number))
                .then(|| self.fetch(*page))
            }));
        let flushed = self.flush()?;
        step.outgoing.extend(flushed.outgoing);
        Ok(step)
    }

    fn fetched(
        &mut self,
        page: SyncPageId,
        branch: Branch,
        membership_revision: u64,
        snapshot_info: Option<SnapshotInfo>,
        snapshot: Option<Vec<u8>>,
        records: Vec<(u64, Vec<u8>)>,
    ) -> Result<Step, Error> {
        if branch != Branch::Body {
            return Err(Error::UnexpectedRecordKind);
        }
        let has_author_keys = self.author_keys_through >= Some(membership_revision);
        let Some(state) = self.pages.get_mut(&page) else {
            return Ok(Step::default());
        };
        if state.halt == Some(Halt::Stalled) {
            return Ok(Step::default());
        }
        let highest = records
            .last()
            .map(|(sequence, _)| *sequence)
            .max(snapshot_info.map(|info| info.last_sequence));
        state.fetched_through = state.fetched_through.max(highest);
        let mut replacements = Vec::new();
        let mut progress = Progress::Continue;
        if let Some(bytes) = snapshot {
            progress = self.receive_snapshot(page, bytes, has_author_keys, &mut replacements);
        }
        if progress == Progress::Continue {
            progress = self.receive_records(page, records, has_author_keys, &mut replacements)?;
        }

        let mut step = Step::default();
        let Some(state) = self.pages.get_mut(&page) else {
            return Ok(step);
        };
        if let Some(info) = snapshot_info
            && state.last_sequence >= Some(info.last_sequence)
        {
            state.remember_snapshot(info);
        }
        if state
            .read_wait
            .is_some_and(|wait| state.last_sequence >= Some(wait.sequence))
        {
            state.read_wait = None;
        }
        match progress {
            Progress::Continue => {}
            Progress::Waiting(wait) => {
                state.read_wait = Some(wait);
                if wait.generation.is_some() && !self.fetching_generations {
                    self.fetching_generations = true;
                    step.outgoing
                        .push(Outgoing::FetchGenerations { topic: self.topic });
                }
            }
            Progress::Stalled => {
                state.halt = Some(Halt::Stalled);
                step.stalled.push(page);
            }
        }
        state.caught_up = progress == Progress::Continue
            && state.read_wait.is_none()
            && state.last_sequence >= state.fetched_through;
        if !replacements.is_empty() {
            step.changes.push(PageChange { page, replacements });
        }
        let flushed = self.flush()?;
        step.outgoing.extend(flushed.outgoing);
        Ok(step)
    }

    /// まとめたスナップショットを確かめ、手元の文書に合わせる。
    ///
    /// 規則に合わないスナップショットは、まとめた範囲のレコードがもう中継に無く、続きを当てると文書が欠けるため、
    /// 読み飛ばさずにページへの適用を止める。
    fn receive_snapshot(
        &mut self,
        page: SyncPageId,
        bytes: Vec<u8>,
        has_author_keys: bool,
        replacements: &mut Vec<Replacement>,
    ) -> Progress {
        let Self {
            topic,
            pages,
            generations,
            unreadable_generations,
            known_keys,
            policy,
            ..
        } = self;
        let topic = *topic;
        let Some(state) = pages.get_mut(&page) else {
            return Progress::Continue;
        };
        let Ok(record) = SealedRecord::parse(bytes) else {
            return Progress::Stalled;
        };
        let header = *record.header();
        let RecordKind::Snapshot { last_sequence } = header.kind else {
            return Progress::Stalled;
        };
        if header.topic != topic || header.page != page || header.branch != Branch::Body {
            return Progress::Stalled;
        }
        if state
            .last_sequence
            .is_some_and(|last| last_sequence <= last)
        {
            return Progress::Continue;
        }
        if !policy.readable.contains(&header.suite) {
            return Progress::Stalled;
        }
        let Some(keys) = generations.get(&header.generation) else {
            return Progress::need_generation(
                unreadable_generations,
                header.generation,
                last_sequence,
            );
        };
        let view = GenerationView {
            generation: header.generation,
            write_keys: keys.write_keys,
            topic_key: keys.role_keys.topic_key(),
        };
        let checked = match check_snapshot(record.into_bytes(), &view, |record_topic, member| {
            author_key(known_keys, topic, record_topic, member)
        }) {
            Ok(checked) => checked,
            Err(Error::UnknownMember) if !has_author_keys => {
                return Progress::Waiting(ReadWait {
                    sequence: last_sequence,
                    generation: None,
                });
            }
            Err(_) => return Progress::Stalled,
        };
        // 履歴を省いたまとめは、手元の編集を持つ文書に合わせられない。
        if checked.retention() != VersionRetention::History {
            return Progress::Stalled;
        }
        let Ok(applied) = state.document.apply_snapshot(checked.document_snapshot()) else {
            return Progress::Stalled;
        };
        replacements.extend(applied);
        state.versions = checked.versions().to_vec();
        state.last_sequence = Some(last_sequence);
        state
            .stored_suites
            .retain(|(sequence, _)| *sequence > last_sequence);
        Progress::Continue
    }

    /// 取得したログのレコードを、順番の順に「中身の検査の規則」で確かめて当てる。
    fn receive_records(
        &mut self,
        page: SyncPageId,
        records: Vec<(u64, Vec<u8>)>,
        has_author_keys: bool,
        replacements: &mut Vec<Replacement>,
    ) -> Result<Progress, Error> {
        let Self {
            topic,
            pages,
            generations,
            unreadable_generations,
            known_keys,
            policy,
            ..
        } = self;
        let topic = *topic;
        let Some(state) = pages.get_mut(&page) else {
            return Ok(Progress::Continue);
        };
        for (sequence, bytes) in records {
            if state.last_sequence.is_some_and(|last| sequence <= last) {
                continue;
            }
            let record = match SealedRecord::parse(bytes) {
                Ok(record) => record,
                Err(err) if stops_page(&err) => return Ok(Progress::Stalled),
                Err(_) => {
                    // 封筒を読めないレコードは、どの端末でも当てないため、読み飛ばす。
                    state.last_sequence = Some(sequence);
                    continue;
                }
            };
            let header = *record.header();
            if header.topic == topic
                && header.page == page
                && header.branch == Branch::Body
                && header.kind == RecordKind::Log
            {
                if !policy.readable.contains(&header.suite) {
                    return Ok(Progress::Stalled);
                }
                let Some(keys) = generations.get(&header.generation) else {
                    return Ok(Progress::need_generation(
                        unreadable_generations,
                        header.generation,
                        sequence,
                    ));
                };
                let view = GenerationView {
                    generation: header.generation,
                    write_keys: keys.write_keys,
                    topic_key: keys.role_keys.topic_key(),
                };
                let checked =
                    check_log_entry(record.into_bytes(), &view, |record_topic, author| {
                        author_key(known_keys, topic, record_topic, author)
                    });
                match checked {
                    Ok(CheckedEntry::Update(update)) => {
                        replacements.extend(state.document.apply_remote(update.update())?);
                    }
                    Ok(CheckedEntry::Version(version)) => state.versions.push(version),
                    Err(Error::UnknownMember) if !has_author_keys => {
                        return Ok(Progress::Waiting(ReadWait {
                            sequence,
                            generation: None,
                        }));
                    }
                    Err(err) if stops_page(&err) => return Ok(Progress::Stalled),
                    // 規則に合わないレコードは、どの端末でも当てないため読み飛ばす。
                    Err(_) => {}
                }
            }
            state.last_sequence = Some(sequence);
            state.stored_suites.push((sequence, header.suite));
        }
        Ok(Progress::Continue)
    }
}

/// 取得したログを、どこまで当てたか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Progress {
    /// 取得したものをすべて確かめた。
    Continue,
    /// 世代か、著者の認証済みの公開鍵がそろうまで待つ。
    Waiting(ReadWait),
    /// 知らない版か方式のレコードか、規則に合わないスナップショットに当たり、ページへの適用を止める。
    Stalled,
}

impl Progress {
    fn need_generation(unreadable: &HashSet<u64>, number: u64, sequence: u64) -> Self {
        if unreadable.contains(&number) {
            Self::Stalled
        } else {
            Self::Waiting(ReadWait {
                sequence,
                generation: Some(number),
            })
        }
    }
}

/// 知らない封筒の版・方式・中身の版で、ページへの適用を止めるエラーか (中身の検査の規則の1と3)。
fn stops_page(err: &Error) -> bool {
    matches!(
        err,
        Error::UnsupportedEnvelopeVersion(_)
            | Error::UnsupportedSuite(_)
            | Error::UnsupportedContentVersion(_)
    )
}

/// 著者の署名を確かめる、認証済みの公開鍵を返す。別のトピックのレコードには返さない。
fn author_key(
    known_keys: &HashMap<MemberId, UserVerifyingKey>,
    topic: TopicId,
    record_topic: TopicId,
    member: MemberId,
) -> Option<UserVerifyingKey> {
    (record_topic == topic)
        .then(|| known_keys.get(&member).copied())
        .flatten()
}

fn is_editor(membership: &Membership, member: MemberId) -> bool {
    membership
        .get(&member)
        .is_some_and(|member| member.role == Role::Editor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::member::Member;
    use crate::record::{CONTENT_VERSION, ENVELOPE_VERSION, MAGIC};

    struct CompatibilityFixture {
        client: SyncClient,
        member: Member,
        keys: UserKeys,
        membership: Membership,
        generation: GenerationKeys,
        topic: TopicId,
        affected: SyncPageId,
        healthy: SyncPageId,
    }

    impl CompatibilityFixture {
        fn new() -> Self {
            let keys = UserKeys::generate(Suite::CURRENT).expect("利用者の鍵を作れなかった");
            let member = Member {
                id: MemberId::generate().expect("メンバーIDを作れなかった"),
                role: Role::Editor,
                keys: keys.public_keys().clone(),
            };
            let membership =
                Membership::new(vec![member.clone()]).expect("メンバー構成を作れなかった");
            let topic = TopicId::generate().expect("トピックIDを作れなかった");
            let affected = SyncPageId::generate().expect("ページIDを作れなかった");
            let healthy = SyncPageId::generate().expect("ページIDを作れなかった");
            let mut client = SyncClient::new(topic, member.id, keys.clone(), membership.clone())
                .expect("同期の処理を作れなかった");
            for page in [affected, healthy] {
                client.open_page(page).expect("ページを開けなかった");
            }
            client.connect();
            Self {
                client,
                member,
                keys,
                membership,
                generation: GenerationKeys::generate().expect("世代の鍵を作れなかった"),
                topic,
                affected,
                healthy,
            }
        }

        fn header(&self, page: SyncPageId, generation: u64) -> RecordHeader {
            RecordHeader {
                kind: RecordKind::Log,
                suite: Suite::CURRENT,
                topic: self.topic,
                page,
                branch: Branch::Body,
                generation,
            }
        }

        fn receive_generations(&mut self, number: u64, mut records: Vec<(u64, Vec<u8>)>) {
            let record = GenerationRecord::create(
                Suite::CURRENT,
                self.topic,
                number,
                &self.generation,
                self.member.id,
                &self.keys,
                &self.membership,
            )
            .expect("世代の記録を作れなかった");
            self.client
                .remember_generation_membership(number, self.membership.clone());
            records.push((number, record.into_bytes()));
            self.client
                .receive(Incoming::Generations { records })
                .expect("世代の取得結果を処理できなかった");
        }

        fn receive_page(&mut self, page: SyncPageId, bytes: Vec<u8>) -> Step {
            self.client
                .receive(Incoming::Fetched {
                    page,
                    branch: Branch::Body,
                    membership_revision: 0,
                    minimum_client_version: CLIENT_VERSION,
                    snapshot_info: None,
                    snapshot: None,
                    records: vec![(1, bytes)],
                })
                .expect("ページの取得結果を処理できなかった")
        }

        fn assert_only_affected_stalls(&mut self, number: u64, unsupported: Vec<u8>) {
            assert_eq!(
                self.receive_page(self.affected, unsupported).stalled,
                vec![self.affected],
                "未対応のページの停止が報告されなかった"
            );
            let before = self.client.document(&self.affected).expect("ページが無い");
            assert!(before.text().is_empty(), "未対応のレコードが本文に入った");
            assert!(
                matches!(self.client.compact(self.affected), Err(Error::PageStalled)),
                "未対応のページをまとめられた"
            );
            assert_eq!(
                self.client
                    .edit(self.affected, 0..0, "手元の編集。")
                    .expect("手元で編集できなかった")
                    .outgoing,
                Vec::new(),
                "未対応のページを中継へ書き戻した"
            );

            let writer = Writer::start(&self.member, &self.keys).expect("書き手を作れなかった");
            let mut document = Document::new("").expect("文書を作れなかった");
            document
                .set_writer(writer.id())
                .expect("書き手を設定できなかった");
            let update = document
                .apply_local(0..0, "読めるページ。")
                .expect("文書を書けなかった")
                .expect("更新が無い");
            let record = writer
                .seal_update(
                    self.header(self.healthy, number),
                    &update,
                    self.generation.topic_key(),
                    self.generation.body_key(),
                )
                .expect("更新を封じられなかった");
            assert_eq!(
                self.receive_page(self.healthy, record.into_bytes()).stalled,
                Vec::new(),
                "読めるページまで止まった"
            );
            assert_eq!(
                self.client
                    .document(&self.healthy)
                    .expect("ページが無い")
                    .text(),
                "読めるページ。",
                "読めるページの更新が当たらなかった"
            );
            assert!(
                matches!(
                    self.client
                        .edit(self.healthy, 0..0, "手元の編集。")
                        .expect("手元で編集できなかった")
                        .outgoing
                        .as_slice(),
                    [Outgoing::Submit { .. }]
                ),
                "読めるページに書き戻せなかった"
            );
        }
    }

    /// 新しい版の封筒の識別部分だけを持つ互換性テストの応答。
    fn future_envelope() -> Vec<u8> {
        [MAGIC.as_slice(), &(ENVELOPE_VERSION + 1).to_be_bytes()].concat()
    }

    #[test]
    fn 知らない封筒の版か中身の版は_そのページだけの同期を止める() {
        for envelope in [false, true] {
            let mut fixture = CompatibilityFixture::new();
            fixture.receive_generations(1, Vec::new());
            let record = if envelope {
                future_envelope()
            } else {
                SealedRecord::seal_with(
                    fixture.header(fixture.affected, 1),
                    b"future content",
                    fixture.generation.topic_key(),
                    fixture.generation.body_key(),
                    CONTENT_VERSION + 1,
                    &[],
                )
                .expect("互換性テスト用の中身を封じられなかった")
                .into_bytes()
            };
            fixture.assert_only_affected_stalls(1, record);
        }
    }

    #[test]
    fn 知らない版の世代と読める世代を同時に取得しても_読めるページは同期する() {
        let mut fixture = CompatibilityFixture::new();
        fixture.receive_generations(2, vec![(1, future_envelope())]);
        let referring_to_old_generation = SealedRecord::seal(
            fixture.header(fixture.affected, 1),
            b"unreadable generation",
            fixture.generation.topic_key(),
            fixture.generation.body_key(),
        )
        .expect("互換性テスト用のレコードを封じられなかった");
        fixture.assert_only_affected_stalls(2, referring_to_old_generation.into_bytes());
    }
}
