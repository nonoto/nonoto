//! 端末と中継を、同じプロセスのメモリの上でつなぐテストの道具。
//!
//! 端末ごとに、中継へ送るメッセージの列 (上り) と、中継から届くメッセージの列 (下り) を持つ。
//! テストは、どの端末のどちらの列を、どの順で何件届けるかを決めて、遅延・順不同・重複・切断を起こす。
//! 乱数や時計は使わず、同じテストは毎回同じ順でメッセージを届ける。

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ops::Range;

use nonoto_core::{
    Incoming, MemberId, Membership, MembershipChange, Outgoing, Step, SyncClient, SyncPageId,
    TopicId, UserVerifyingKey,
};

use crate::{Rejection, Relay};

/// [`Network::run`] が、メッセージが尽きるまでに届ける件数の上限。超えたら同期の処理が止まらないとみなす。
const MAX_DELIVERIES: usize = 100_000;

/// テストの道具の操作の失敗。
#[derive(Debug, thiserror::Error)]
pub enum NetworkError {
    /// 端末の同期の処理が失敗した。
    #[error("端末の同期の処理が失敗した")]
    Client(#[source] nonoto_core::Error),

    /// 中継がメンバー構成の変更を受け付けなかった。
    #[error("中継がメンバー構成の変更を受け付けなかった")]
    Relay(#[source] Rejection),

    /// メッセージを届け続けても、送るメッセージが尽きなかった。
    #[error("メッセージを{MAX_DELIVERIES}件届けても、送るメッセージが尽きなかった")]
    NotQuiet,
}

impl From<nonoto_core::Error> for NetworkError {
    fn from(error: nonoto_core::Error) -> Self {
        Self::Client(error)
    }
}

/// つないだ端末の番号。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceId(usize);

#[derive(Debug)]
struct Device {
    account: MemberId,
    client: SyncClient,
    uplink: VecDeque<Outgoing>,
    downlink: VecDeque<Incoming>,
    connected: bool,
}

#[derive(Debug, Default)]
struct AuthorKeys {
    keys: HashMap<MemberId, UserVerifyingKey>,
    /// 初期構成から途切れずに記録した最後の更新番号。
    through_revision: Option<u64>,
}

impl AuthorKeys {
    fn remember(&mut self, membership: &Membership, revision: u64) {
        self.keys.extend(
            membership
                .iter()
                .map(|member| (member.id, member.keys.signing)),
        );
        if revision == 0
            || self.through_revision.and_then(|last| last.checked_add(1)) == Some(revision)
        {
            self.through_revision = Some(revision);
        }
    }
}

/// 端末と中継をつないだもの。
#[derive(Debug)]
pub struct Network {
    relay: Relay,
    devices: Vec<Device>,
    /// 世代の受付時点の構成を、テストの認証済みの入力として端末に渡す。
    generation_memberships: HashMap<TopicId, BTreeMap<u64, Membership>>,
    /// 退会したメンバーを含む著者の公開鍵を、構成変更のたびに記録する。
    author_keys: HashMap<TopicId, AuthorKeys>,
}

impl Network {
    /// 渡した中継に端末をつなぐための、端末がまだ無いテストの道具を作る。
    pub fn new(relay: Relay) -> Self {
        let generation_memberships = relay
            .topics
            .iter()
            .filter(|(_, state)| state.membership_revision == 0)
            .map(|(topic, state)| {
                (
                    *topic,
                    state
                        .generations
                        .keys()
                        .map(|number| (*number, state.membership.clone()))
                        .collect(),
                )
            })
            .collect();
        let author_keys = relay
            .topics
            .iter()
            .map(|(topic, state)| {
                let mut keys = AuthorKeys::default();
                keys.remember(&state.membership, state.membership_revision);
                (*topic, keys)
            })
            .collect();
        Self {
            relay,
            devices: Vec::new(),
            generation_memberships,
            author_keys,
        }
    }

    /// このテストの道具に接続した中継の状態を読む。
    pub fn relay(&self) -> &Relay {
        &self.relay
    }

    /// 中継の設定 (最低限のクライアントの版・受け付けなくする方式) を変える。
    pub fn relay_mut(&mut self) -> &mut Relay {
        &mut self.relay
    }

    /// 端末を足す。まだつながっていない。
    pub fn add_device(&mut self, account: MemberId, client: SyncClient) -> DeviceId {
        let id = DeviceId(self.devices.len());
        self.devices.push(Device {
            account,
            client,
            uplink: VecDeque::new(),
            downlink: VecDeque::new(),
            connected: false,
        });
        self.update_client_membership(id);
        id
    }

    /// このテストの道具で足した端末の、現在の同期状態を読む。
    pub fn client(&self, device: DeviceId) -> &SyncClient {
        &self.devices[device.0].client
    }

    /// 上りと下りの列に残っているメッセージの数を返す。
    pub fn queued(&self, device: DeviceId) -> (usize, usize) {
        let device = &self.devices[device.0];
        (device.uplink.len(), device.downlink.len())
    }

    /// 現在の構成と著者の公開鍵を渡し、端末を中継につなぐ。
    pub fn connect(&mut self, device: DeviceId) {
        self.update_client_membership(device);
        let device = &mut self.devices[device.0];
        device.connected = true;
        let step = device.client.connect();
        device.uplink.extend(step.outgoing);
    }

    /// 端末の接続を切る。上りと下りの列にあるメッセージは失われる。
    pub fn disconnect(&mut self, device: DeviceId) {
        let device = &mut self.devices[device.0];
        device.connected = false;
        device.uplink.clear();
        device.downlink.clear();
        device.client.disconnect();
    }

    /// このテストの道具で足した端末のページを開く。接続中なら取得の求めを列に加える。
    pub fn open_page(&mut self, device: DeviceId, page: SyncPageId) -> Result<(), NetworkError> {
        let step = self.devices[device.0].client.open_page(page)?;
        self.enqueue(device, step);
        Ok(())
    }

    /// 端末で手元の編集をする。送れるものは上りの列に入る。
    pub fn edit(
        &mut self,
        device: DeviceId,
        page: SyncPageId,
        range: Range<usize>,
        text: &str,
    ) -> Result<(), NetworkError> {
        let step = self.devices[device.0].client.edit(page, range, text)?;
        self.enqueue(device, step);
        Ok(())
    }

    /// 端末で、手元の文書の今の時点に版を付ける。
    pub fn create_version(
        &mut self,
        device: DeviceId,
        page: SyncPageId,
    ) -> Result<(), NetworkError> {
        let step = self.devices[device.0].client.create_version(page)?;
        self.enqueue(device, step);
        Ok(())
    }

    /// 端末に、ページの本文のログをまとめさせる。
    pub fn compact(&mut self, device: DeviceId, page: SyncPageId) -> Result<(), NetworkError> {
        let step = self.devices[device.0].client.compact(page)?;
        self.enqueue(device, step);
        Ok(())
    }

    /// トピックの本文のログを同期に登録し、古い方式の暗号文が残るページをまとめ直させる。
    pub fn rewrite_old_pages(&mut self, device: DeviceId) -> Result<(), NetworkError> {
        let topic = self.devices[device.0].client.topic();
        for page in self.relay.body_pages(&topic) {
            self.open_page(device, page)?;
        }
        let step = self.devices[device.0].client.rewrite_old_pages()?;
        self.enqueue(device, step);
        Ok(())
    }

    /// 端末に、ほかの端末の更新を取りに行かせる。
    pub fn poll(&mut self, device: DeviceId) {
        let step = self.devices[device.0].client.poll();
        self.enqueue(device, step);
    }

    /// メンバー構成の変更を、中継と、渡した端末に知らせる。外した端末には知らせない。
    pub fn change_membership(
        &mut self,
        membership: &Membership,
        change: MembershipChange,
        topic: TopicId,
        devices: &[DeviceId],
    ) -> Result<(), NetworkError> {
        self.relay
            .change_membership(topic, membership.clone(), change)
            .map_err(NetworkError::Relay)?;
        self.notify_membership(topic, devices)
    }

    fn notify_membership(
        &mut self,
        topic: TopicId,
        devices: &[DeviceId],
    ) -> Result<(), NetworkError> {
        let (membership, revision, _) = self
            .relay
            .membership_status(topic)
            .ok_or(NetworkError::Relay(Rejection::UnknownTopic))?;
        self.author_keys
            .entry(topic)
            .or_default()
            .remember(membership, revision);
        for id in devices {
            let device = &mut self.devices[id.0];
            if device.client.topic() != topic {
                continue;
            }
            device
                .uplink
                .retain(|message| !matches!(message, Outgoing::Submit { .. }));
            self.update_client_membership(*id);
        }
        Ok(())
    }

    fn update_client_membership(&mut self, id: DeviceId) {
        let client = &mut self.devices[id.0].client;
        let topic = client.topic();
        let Some((membership, revision, closed_through)) = self.relay.membership_status(topic)
        else {
            return;
        };
        if let Some(keys) = self.author_keys.get(&topic) {
            client.remember_author_keys(
                keys.keys.iter().map(|(&id, &key)| (id, key)),
                keys.through_revision,
            );
        }
        let step = client.change_membership(membership.clone(), revision, closed_through);
        self.enqueue(id, step);
    }

    /// 上りの列の先頭を中継に届け、答えを下りの列の末尾に入れる。届けたら真を返す。
    pub fn send_one(&mut self, device: DeviceId) -> bool {
        let device = &mut self.devices[device.0];
        if !device.connected {
            return false;
        }
        let Some(message) = device.uplink.pop_front() else {
            return false;
        };
        let topic = device.client.topic();
        let before = self.relay.last_generation(topic);
        let answer = self.relay.handle(&device.account, message);
        if let Some(number) = self.relay.last_generation(topic)
            && Some(number) != before
            && let Some((membership, _, _)) = self.relay.membership_status(topic)
        {
            self.generation_memberships
                .entry(topic)
                .or_default()
                .insert(number, membership.clone());
        }
        device.downlink.push_back(answer);
        true
    }

    /// 下りの列の先頭を端末に届け、端末が送るメッセージを上りの列に入れる。届けたら真を返す。
    pub fn receive_one(&mut self, device: DeviceId) -> Result<bool, NetworkError> {
        let state = &mut self.devices[device.0];
        let Some(message) = state.downlink.pop_front() else {
            return Ok(false);
        };
        if matches!(&message, Incoming::Generations { .. })
            && let Some(memberships) = self.generation_memberships.get(&state.client.topic())
        {
            for (number, membership) in memberships {
                state
                    .client
                    .remember_generation_membership(*number, membership.clone());
            }
        }
        let step = state.client.receive(message)?;
        self.enqueue(device, step);
        Ok(true)
    }

    /// 下りの列の先頭のメッセージを重ねる (中継の答えが2回届く)。
    pub fn duplicate_downlink(&mut self, device: DeviceId) {
        let downlink = &mut self.devices[device.0].downlink;
        if let Some(front) = downlink.front().cloned() {
            downlink.push_front(front);
        }
    }

    /// 下りの列の順を逆にする (中継の答えが順不同で届く)。
    pub fn reverse_downlink(&mut self, device: DeviceId) {
        let downlink = &mut self.devices[device.0].downlink;
        let reversed: VecDeque<Incoming> = downlink.drain(..).rev().collect();
        *downlink = reversed;
    }

    /// 上りの列のメッセージをすべて中継に届ける。答えは下りの列に溜まり、端末にはまだ届けない。
    pub fn send_all(&mut self, device: DeviceId) {
        while self.send_one(device) {}
    }

    /// つないだ端末の、上りと下りの列が尽きるまで、端末の順に1件ずつ届ける。
    pub fn run(&mut self) -> Result<(), NetworkError> {
        self.run_with_limit(MAX_DELIVERIES)
    }

    fn run_with_limit(&mut self, limit: usize) -> Result<(), NetworkError> {
        let mut delivered = 0;
        loop {
            let previous = delivered;
            for index in 0..self.devices.len() {
                if delivered == limit {
                    break;
                }
                let device = DeviceId(index);
                delivered += usize::from(self.send_one(device));
                if delivered < limit && self.devices[index].connected {
                    delivered += usize::from(self.receive_one(device)?);
                }
            }
            if delivered == limit
                && self.devices.iter().any(|device| {
                    device.connected && !(device.uplink.is_empty() && device.downlink.is_empty())
                })
            {
                return Err(NetworkError::NotQuiet);
            }
            if delivered == previous {
                return Ok(());
            }
        }
    }

    /// つないだ端末すべてに更新を取りに行かせ、列が尽きるまで届けることを2回繰り返す。
    ///
    /// 1回目で届いた更新に続く、ほかの端末の送り直しや新しい世代を、2回目で取り込む。
    pub fn settle(&mut self) -> Result<(), NetworkError> {
        for _ in 0..2 {
            for index in 0..self.devices.len() {
                if self.devices[index].connected {
                    self.poll(DeviceId(index));
                }
            }
            self.run()?;
        }
        Ok(())
    }

    fn enqueue(&mut self, device: DeviceId, step: Step) {
        let device = &mut self.devices[device.0];
        if device.connected {
            device.uplink.extend(step.outgoing);
        }
    }
}

#[cfg(test)]
mod tests {
    use nonoto_core::{
        Branch, BranchId, CLIENT_VERSION, Document, GenerationRecord, Member, PageChange,
        RecordHeader, RecordKind, RejectReason, Replacement, Role, SealedRecord, SnapshotInfo,
        Suite, SuitePolicy, UserKeys, Writer,
    };

    use super::*;
    use crate::LogKey;

    /// 「テストのメンバー構成」のA (編集。2台)・B (編集)・C (提案)・D (閲覧)。
    struct Fixture {
        users: Vec<(MemberId, UserKeys)>,
        roles: Vec<Role>,
        topic: TopicId,
        page: SyncPageId,
    }

    const A: usize = 0;
    const B: usize = 1;
    const C: usize = 2;
    const D: usize = 3;

    impl Fixture {
        fn new() -> Self {
            let roles = vec![Role::Editor, Role::Editor, Role::Proposer, Role::Viewer];
            Self {
                users: roles
                    .iter()
                    .map(|_| {
                        (
                            MemberId::generate().expect("メンバーのIDを作れなかった"),
                            UserKeys::generate(Suite::CURRENT).expect("利用者の鍵を作れなかった"),
                        )
                    })
                    .collect(),
                roles,
                topic: TopicId::generate().expect("トピックのIDを作れなかった"),
                page: SyncPageId::generate().expect("ページのIDを作れなかった"),
            }
        }

        /// `members` の利用者だけのメンバー構成。
        fn membership(&self, members: &[usize]) -> Membership {
            Membership::new(
                members
                    .iter()
                    .map(|index| Member {
                        id: self.users[*index].0,
                        role: self.roles[*index],
                        keys: self.users[*index].1.public_keys().clone(),
                    })
                    .collect(),
            )
            .expect("メンバー構成を作れなかった")
        }

        fn network(&self) -> Network {
            let mut relay = Relay::new();
            relay
                .add_topic(self.topic, self.membership(&[A, B, C, D]))
                .expect("トピックを預けられなかった");
            Network::new(relay)
        }

        /// 利用者の端末を足し、つないでページを開く。
        fn join(&self, network: &mut Network, user: usize) -> DeviceId {
            self.join_with(network, user, self.membership(&[A, B, C, D]))
        }

        fn join_with(
            &self,
            network: &mut Network,
            user: usize,
            membership: Membership,
        ) -> DeviceId {
            self.join_as(network, user, membership, SuitePolicy::current())
        }

        /// 読み書きできる方式とクライアントの版を `policy` にした端末を足し、つないでページを開く。
        fn join_policy(&self, network: &mut Network, user: usize, policy: SuitePolicy) -> DeviceId {
            self.join_as(network, user, self.membership(&[A, B, C, D]), policy)
        }

        fn join_as(
            &self,
            network: &mut Network,
            user: usize,
            membership: Membership,
            policy: SuitePolicy,
        ) -> DeviceId {
            let (id, keys) = &self.users[user];
            let client = SyncClient::new(self.topic, *id, keys.clone(), membership)
                .expect("同期の処理を作れなかった")
                .with_policy(policy);
            let device = network.add_device(*id, client);
            network.connect(device);
            network
                .open_page(device, self.page)
                .expect("ページを開けなかった");
            device
        }

        fn text(&self, network: &Network, device: DeviceId) -> String {
            page_text(network, device, self.page)
        }

        fn append(&self, network: &mut Network, device: DeviceId, text: &str) {
            let end = self.text(network, device).len();
            network
                .edit(device, self.page, end..end, text)
                .expect("手元の編集ができなかった");
        }

        fn body_log(&self, page: SyncPageId) -> LogKey {
            LogKey {
                topic: self.topic,
                page,
                branch: Branch::Body,
            }
        }

        /// 初代の鍵を配られた編集者が持つ鍵で、指定したログのテスト用の更新を封じる。
        fn signed_update(
            &self,
            network: &Network,
            author: usize,
            page: SyncPageId,
            branch: Branch,
            text: &str,
        ) -> Vec<u8> {
            let (id, author_keys) = &self.users[author];
            let member = Member {
                id: *id,
                role: self.roles[author],
                keys: author_keys.public_keys().clone(),
            };
            self.sealed_update(network, &member, author_keys, page, branch, text)
        }

        /// [`Fixture::signed_update`] と同じ鍵で、`member` を著者にした更新を封じる。著者はメンバー構成にいなくてよい。
        fn sealed_update(
            &self,
            network: &Network,
            member: &Member,
            author_keys: &UserKeys,
            page: SyncPageId,
            branch: Branch,
            text: &str,
        ) -> Vec<u8> {
            let record = network.relay().generations(&self.topic).remove(0);
            let record = GenerationRecord::parse(record).expect("世代の記録を読めなかった");
            let membership = &network.generation_memberships[&self.topic][&1];
            let (editor, editor_keys) = &self.users[A];
            let keys = record
                .open(*editor, editor_keys, membership)
                .expect("初代の鍵を開けなかった");
            let writer = Writer::start(member, author_keys).expect("書き手を作れなかった");
            let mut document = Document::new("").expect("文書を作れなかった");
            document
                .set_writer(writer.id())
                .expect("書き手を設定できなかった");
            let update = document
                .apply_local(0..0, text)
                .expect("文書を書けなかった")
                .expect("更新が無い");
            let write_key = match branch {
                Branch::Body => keys.body_key(),
                Branch::Proposal(_) => keys.proposal_key(),
            }
            .expect("書き込みの鍵を持たない");
            writer
                .seal_update(
                    RecordHeader {
                        kind: RecordKind::Log,
                        suite: Suite::CURRENT,
                        topic: self.topic,
                        page,
                        branch,
                        generation: 1,
                    },
                    &update,
                    keys.topic_key(),
                    write_key,
                )
                .expect("レコードを封じられなかった")
                .into_bytes()
        }
    }

    #[test]
    fn 後から参加した端末が_ほかの端末と同じ文書になる() {
        let fixture = Fixture::new();
        let mut network = fixture.network();
        let a = fixture.join(&mut network, A);
        fixture.append(&mut network, a, "Aの本文。");
        network.run().expect("同期が止まらなかった");
        let b = fixture.join(&mut network, B);
        network.run().expect("同期が止まらなかった");
        fixture.append(&mut network, b, "Bの追記。");
        network.settle().expect("同期が止まらなかった");

        let d = fixture.join(&mut network, D);
        network.settle().expect("同期が止まらなかった");

        let expected = fixture.text(&network, a);
        assert_eq!(
            fixture.text(&network, b),
            expected,
            "Bの文書がAと食い違った"
        );
        assert_eq!(
            fixture.text(&network, d),
            expected,
            "後から参加したDの文書がAと食い違った"
        );
        assert!(
            expected.contains("Aの本文。") && expected.contains("Bの追記。"),
            "どちらかの端末の書き込みが失われた: {expected}",
        );
    }

    #[test]
    fn 順不同と重複で届いても_同時の編集が収束する() {
        let fixture = Fixture::new();
        let mut network = fixture.network();
        let first = fixture.join(&mut network, A);
        fixture.append(&mut network, first, "最初。");
        network.run().expect("同期が止まらなかった");
        let second = fixture.join(&mut network, A);
        let b = fixture.join(&mut network, B);
        let d = fixture.join(&mut network, D);
        network.settle().expect("同期が止まらなかった");

        // 同じ利用者の2台とBが、互いの更新を受け取る前に書く。
        fixture.append(&mut network, first, "1台目。");
        fixture.append(&mut network, second, "2台目。");
        fixture.append(&mut network, b, "B。");
        for device in [first, second, b] {
            network.send_all(device);
        }
        // 中継の答えを、1台目には重ねて、2台目には逆の順で届ける。
        network.duplicate_downlink(first);
        network.reverse_downlink(second);
        network.settle().expect("同期が止まらなかった");

        let expected = fixture.text(&network, first);
        for device in [second, b, d] {
            assert_eq!(
                fixture.text(&network, device),
                expected,
                "端末の文書が食い違った"
            );
        }
        for text in ["1台目。", "2台目。", "B。"] {
            assert!(
                expected.contains(text),
                "{text}の書き込みが失われた: {expected}"
            );
        }
    }

    #[test]
    fn 切断した端末が_つなぎ直して続きを取得し_送っていない更新を送る() {
        let fixture = Fixture::new();
        let mut network = fixture.network();
        let a = fixture.join(&mut network, A);
        let b = fixture.join(&mut network, B);
        fixture.append(&mut network, a, "最初。");
        network.settle().expect("同期が止まらなかった");

        // Aは更新を中継に届けたが、受け付けの答えを受け取る前に切れる。切れているあいだも書く。
        fixture.append(&mut network, a, "切れる前。");
        network.send_all(a);
        network.disconnect(a);
        fixture.append(&mut network, a, "切れているあいだ。");
        fixture.append(&mut network, b, "Bの追記。");
        network.settle().expect("同期が止まらなかった");
        assert_eq!(
            network.client(a).pending(&fixture.page),
            2,
            "切れているあいだに、受け付けを知らない更新が減った",
        );

        network.connect(a);
        network.send_all(a);
        network.reverse_downlink(a);
        network.run().expect("再接続後の同期が止まらなかった");
        assert_eq!(
            network.client(a).pending(&fixture.page),
            0,
            "ログが世代より先に届いたとき、追加の取得をしないと再送されなかった",
        );
        network.settle().expect("同期が止まらなかった");

        let expected = fixture.text(&network, b);
        assert_eq!(
            fixture.text(&network, a),
            expected,
            "つなぎ直したAの文書がBと食い違った"
        );
        for text in ["切れる前。", "切れているあいだ。", "Bの追記。"] {
            assert!(
                expected.contains(text),
                "{text}の書き込みが失われた: {expected}"
            );
        }
        assert_eq!(
            network.client(a).pending(&fixture.page),
            0,
            "つなぎ直したあとも、受け付けを知らない更新が残った",
        );
    }

    #[test]
    fn 別のページや枝のレコードを本文に当てない() {
        let fixture = Fixture::new();
        let mut network = fixture.network();
        let a = fixture.join(&mut network, A);
        let d = fixture.join(&mut network, D);
        fixture.append(&mut network, a, "元の本文。");
        network.settle().expect("同期が止まらなかった");
        let other_page = SyncPageId::generate().expect("別のページのIDを作れなかった");
        let proposal = Branch::Proposal(BranchId::generate().expect("枝のIDを作れなかった"));
        let other = fixture.signed_update(&network, A, other_page, Branch::Body, "別のページ。");
        let branch = fixture.signed_update(&network, C, fixture.page, proposal, "提案。");

        let step = network.devices[d.0]
            .client
            .receive(Incoming::Fetched {
                page: fixture.page,
                branch: Branch::Body,
                membership_revision: 0,
                snapshot_info: None,
                minimum_client_version: CLIENT_VERSION,
                snapshot: None,
                records: vec![(2, other), (3, branch)],
            })
            .expect("取得結果を処理できなかった");
        assert!(
            step.changes.is_empty(),
            "別のログのレコードで文書に変更が起きた"
        );
        assert_eq!(
            fixture.text(&network, d),
            "元の本文。",
            "別のログのレコードが本文に当たった"
        );
    }

    #[test]
    fn 著者鍵が遅れて届いてもその更新を取り込める() {
        let fixture = Fixture::new();
        let initial = fixture.membership(&[A, D]);
        let mut relay = Relay::new();
        relay
            .add_topic(fixture.topic, initial.clone())
            .expect("トピックを預けられなかった");
        let mut network = Network::new(relay);
        let a = fixture.join_with(&mut network, A, initial.clone());
        let d = fixture.join_with(&mut network, D, initial);
        fixture.append(&mut network, a, "元の本文。");
        network.settle().expect("同期が止まらなかった");

        let members = fixture.membership(&[A, B, D]);
        network
            .change_membership(&members, MembershipChange::Extended, fixture.topic, &[a])
            .expect("メンバー構成を更新できなかった");
        let update =
            fixture.signed_update(&network, B, fixture.page, Branch::Body, "追加した著者。");
        network
            .relay
            .submit(&fixture.users[B].0, CLIENT_VERSION, update)
            .expect("追加した著者の更新を中継が拒んだ");

        network.settle().expect("同期が止まらなかった");
        assert!(
            !fixture.text(&network, d).contains("追加した著者。"),
            "鍵を知る前に更新を当てた"
        );
        assert!(
            fixture.text(&network, a).contains("追加した著者。"),
            "鍵を知る端末で更新を当てられなかった"
        );
        network
            .notify_membership(fixture.topic, &[d])
            .expect("遅れた通知を届けられなかった");
        network.run().expect("通知後の同期が止まらなかった");
        assert_eq!(
            fixture.text(&network, d),
            fixture.text(&network, a),
            "鍵の通知後に取得を再開できなかった"
        );
    }

    #[test]
    fn 世代の途中で参加して退会した著者の更新を_あとから取得できる() {
        let fixture = Fixture::new();
        let remaining = fixture.membership(&[A, D]);
        let mut relay = Relay::new();
        relay
            .add_topic(fixture.topic, remaining.clone())
            .expect("トピックを預けられなかった");
        let mut network = Network::new(relay);
        let a = fixture.join_with(&mut network, A, remaining.clone());
        let d = fixture.join_with(&mut network, D, remaining.clone());
        fixture.append(&mut network, a, "元の本文。");
        network.settle().expect("最初の同期が止まらなかった");
        network.disconnect(d);

        network
            .change_membership(
                &fixture.membership(&[A, B, D]),
                MembershipChange::Extended,
                fixture.topic,
                &[a],
            )
            .expect("著者を追加できなかった");
        let update = fixture.signed_update(&network, B, fixture.page, Branch::Body, "Bの追記。");
        network
            .relay
            .submit(&fixture.users[B].0, CLIENT_VERSION, update)
            .expect("追加した著者の更新を中継が拒んだ");
        network.settle().expect("著者の更新の同期が止まらなかった");

        network
            .change_membership(&remaining, MembershipChange::Reduced, fixture.topic, &[a])
            .expect("著者を外せなかった");
        let new_device = fixture.join_with(&mut network, D, remaining);
        network.connect(d);
        network.run().expect("履歴の取得が止まらなかった");

        assert_eq!(
            network.relay.last_generation(fixture.topic),
            Some(1),
            "著者が参加している間に新しい世代が作られた"
        );
        let expected = fixture.text(&network, a);
        assert!(
            expected.contains("元の本文。") && expected.contains("Bの追記。"),
            "著者を知っている端末が両方の更新を取り込めなかった: {expected}"
        );
        assert_eq!(
            fixture.text(&network, new_device),
            expected,
            "退会後に同期を始めた端末が更新を取り込めなかった"
        );
        assert_eq!(
            fixture.text(&network, d),
            expected,
            "参加と退会の通知を逃した端末が再接続時に更新を取り込めなかった"
        );
    }

    #[test]
    fn メンバー構成が中継に追いついても著者が分からない更新は読み飛ばす() {
        let fixture = Fixture::new();
        let initial = fixture.membership(&[A, B, D]);
        let mut relay = Relay::new();
        relay
            .add_topic(fixture.topic, initial.clone())
            .expect("トピックを預けられなかった");
        let mut network = Network::new(relay);
        let a = fixture.join_with(&mut network, A, initial.clone());
        let b = fixture.join_with(&mut network, B, initial);
        fixture.append(&mut network, a, "元の本文。");
        network.settle().expect("同期が止まらなかった");

        // メンバーでない著者のIDを入れた更新を、本文の書き込みの鍵で署名して中継に書く。
        let outsider_keys = UserKeys::generate(Suite::CURRENT).expect("利用者の鍵を作れなかった");
        let outsider = Member {
            id: MemberId::generate().expect("メンバーのIDを作れなかった"),
            role: Role::Editor,
            keys: outsider_keys.public_keys().clone(),
        };
        let forged = fixture.sealed_update(
            &network,
            &outsider,
            &outsider_keys,
            fixture.page,
            Branch::Body,
            "メンバーでない著者。",
        );
        network
            .relay
            .submit(&fixture.users[A].0, CLIENT_VERSION, forged)
            .expect("本文の書き込みの鍵で署名した更新を中継が拒んだ");

        // Cを足す変更をAにだけ知らせる。Bは、中継より古いメンバー構成のまま取得する。
        network
            .change_membership(
                &fixture.membership(&[A, B, C, D]),
                MembershipChange::Extended,
                fixture.topic,
                &[a],
            )
            .expect("メンバー構成を更新できなかった");
        network.settle().expect("同期が止まらなかった");
        fixture.append(&mut network, b, "Bの追記。");
        fixture.append(&mut network, a, "Aの追記。");
        network.settle().expect("同期が止まらなかった");
        assert_eq!(
            fixture.text(&network, a),
            "元の本文。Aの追記。",
            "構成が中継に追いついた端末が、著者の分からない更新で止まった",
        );
        assert_eq!(
            network.client(b).pending(&fixture.page),
            1,
            "通知を受けていない端末が、著者の鍵を待たずに送った",
        );
        assert!(
            !fixture.text(&network, b).contains("Aの追記。"),
            "通知を受けていない端末が、著者の鍵を待たずに後の更新を当てた",
        );

        network
            .notify_membership(fixture.topic, &[b])
            .expect("遅れた通知を届けられなかった");
        network.settle().expect("通知後の同期が止まらなかった");
        let expected = fixture.text(&network, a);
        assert_eq!(
            fixture.text(&network, b),
            expected,
            "通知後のBの文書がAと食い違った"
        );
        assert_eq!(
            network.client(b).pending(&fixture.page),
            0,
            "通知後もBの編集が送られなかった"
        );
        assert!(
            !expected.contains("メンバーでない著者。"),
            "メンバーでない著者の更新を当てた: {expected}",
        );
        for text in ["Aの追記。", "Bの追記。"] {
            assert!(
                expected.contains(text),
                "{text}の書き込みが失われた: {expected}"
            );
        }
    }

    #[test]
    fn 同じページを開き直しても未送信の編集を保つ() {
        let fixture = Fixture::new();
        let mut network = fixture.network();
        let a = fixture.join(&mut network, A);
        network.disconnect(a);
        fixture.append(&mut network, a, "未送信。");
        network
            .open_page(a, fixture.page)
            .expect("同じページを開けなかった");
        assert_eq!(
            network.client(a).pending(&fixture.page),
            1,
            "未送信の更新を失った"
        );
        assert_eq!(
            fixture.text(&network, a),
            "未送信。",
            "ページを開き直すと本文が消えた"
        );
        network.connect(a);
        network.send_all(a);
        network.reverse_downlink(a);
        network.run().expect("最初の接続の同期が止まらなかった");
        assert_eq!(
            network.client(a).pending(&fixture.page),
            0,
            "世代の無い中継への初回送信が止まった"
        );
        let b = fixture.join(&mut network, B);
        network.run().expect("読み手の同期が止まらなかった");
        assert_eq!(
            fixture.text(&network, b),
            "未送信。",
            "保った編集が相手に届かなかった"
        );
    }

    #[test]
    fn 閲覧と提案の役割で本文の編集を拒んでも文書を変えない() {
        let fixture = Fixture::new();
        let mut network = fixture.network();
        let a = fixture.join(&mut network, A);
        let proposer = fixture.join(&mut network, C);
        let viewer = fixture.join(&mut network, D);
        fixture.append(&mut network, a, "本文。");
        network.settle().expect("同期が止まらなかった");
        for device in [proposer, viewer] {
            for connected in [true, false] {
                if !connected {
                    network.disconnect(device);
                }
                assert!(
                    matches!(
                        network.edit(device, fixture.page, 0..0, "書き込み。"),
                        Err(NetworkError::Client(nonoto_core::Error::NotEditor))
                    ),
                    "本文を書けない役割の編集を受け付けた",
                );
                assert_eq!(
                    fixture.text(&network, device),
                    "本文。",
                    "拒んだ編集で本文が変わった"
                );
                assert_eq!(
                    network.client(device).pending(&fixture.page),
                    0,
                    "拒んだ編集をキューに残した"
                );
            }
        }
    }

    #[test]
    fn 先の編集が恒久拒否されたら後の編集を送らず手元に残す() {
        let fixture = Fixture::new();
        let mut network = fixture.network();
        let a = fixture.join(&mut network, A);
        fixture.append(&mut network, a, "元の本文。");
        network.settle().expect("同期が止まらなかった");
        fixture.append(&mut network, a, "最初。");
        fixture.append(&mut network, a, "次。");
        assert_eq!(network.queued(a).0, 1, "先の編集の受付を待たずに次も送った");
        assert!(
            !network.client(a).is_blocked(&fixture.page),
            "拒まれる前から送信を止めていた"
        );
        let Some(Outgoing::Submit { request, .. }) = network.devices[a.0].uplink.pop_front() else {
            panic!("送る更新が無い");
        };
        let step = network.devices[a.0]
            .client
            .receive(Incoming::Rejected {
                request,
                reason: RejectReason::Other,
            })
            .expect("拒否を処理できなかった");
        assert_eq!(
            step.blocked,
            [fixture.page],
            "送信を止めたページを呼び出し側へ返さなかった"
        );
        assert!(step.outgoing.is_empty(), "拒否の後の更新を送った");
        network.settle().expect("同期待ちが止まらなかった");
        assert!(
            network.client(a).is_blocked(&fixture.page),
            "恒久拒否のあと、ページの送信が止まっていると読めない"
        );
        assert_eq!(
            network.client(a).pending(&fixture.page),
            2,
            "拒まれた操作か依存した操作を捨てた"
        );
        assert_eq!(
            fixture.text(&network, a),
            "元の本文。最初。次。",
            "手元の編集を失った"
        );
    }

    #[test]
    fn メンバーを外したら_編集の端末が新しい世代を作って配り_外したメンバーには渡さない() {
        let fixture = Fixture::new();
        let mut network = fixture.network();
        let a = fixture.join(&mut network, A);
        let b = fixture.join(&mut network, B);
        let d = fixture.join(&mut network, D);
        fixture.append(&mut network, a, "外す前。");
        network.settle().expect("同期が止まらなかった");
        assert_eq!(
            fixture.text(&network, d),
            "外す前。",
            "外す前のDの文書が食い違った"
        );

        // Dを外す。Dの端末には知らせず、切る。
        network.disconnect(d);
        network
            .change_membership(
                &fixture.membership(&[A, B, C]),
                MembershipChange::Reduced,
                fixture.topic,
                &[a, b],
            )
            .expect("メンバー構成の変更を知らせられなかった");
        fixture.append(&mut network, a, "外したあと。");
        network.settle().expect("同期が止まらなかった");

        let generations = network.relay().generations(&fixture.topic);
        assert_eq!(
            generations.len(),
            2,
            "メンバーを外したあとに新しい世代が作られなかった"
        );
        let latest =
            GenerationRecord::parse(generations[1].clone()).expect("世代の記録を読めなかった");
        assert!(
            latest
                .recipients()
                .all(|recipient| *recipient != fixture.users[D].0),
            "外したメンバーに新しい世代の鍵を包んだ",
        );
        assert_eq!(
            network.client(a).current_generation(),
            Some(2),
            "Aが新しい世代で送っていない"
        );
        assert_eq!(
            fixture.text(&network, b),
            "外す前。外したあと。",
            "新しい世代で送った更新をBが当てられなかった",
        );
        assert_eq!(
            fixture.text(&network, d),
            "外す前。",
            "外したDに、新しい世代の更新が届いた"
        );
    }

    #[test]
    fn 変更前に受け付けた世代の通知が遅れても新しい世代で送る() {
        let fixture = Fixture::new();
        let mut network = fixture.network();
        let a = fixture.join(&mut network, A);
        network.run().expect("最初の取得が止まらなかった");
        fixture.append(&mut network, a, "先に書いた本文。");
        assert!(network.send_one(a), "最初の世代を中継へ送れなかった");
        assert_eq!(
            network.relay.last_generation(fixture.topic),
            Some(1),
            "変更前に世代が受け付けられなかった"
        );
        let membership = fixture.membership(&[A, B, C]);
        network
            .change_membership(&membership, MembershipChange::Reduced, fixture.topic, &[a])
            .expect("メンバーを外せなかった");

        assert!(
            network
                .receive_one(a)
                .expect("遅れた受付を処理できなかった"),
            "世代の受付が届かなかった"
        );
        assert_eq!(
            network.client(a).current_generation(),
            None,
            "遅れて届いた旧世代を送信先にした"
        );
        network.run().expect("変更後の同期が止まらなかった");

        let records = network
            .relay()
            .fetch(&fixture.body_log(fixture.page), None)
            .records;
        assert_eq!(records.len(), 1, "変更前の世代で更新を送り出した");
        let record = SealedRecord::parse(records.into_iter().next().expect("更新が無い").1)
            .expect("更新の封筒を読めなかった");
        assert_eq!(record.header().generation, 2, "閉じた世代で更新を送った");
        let members = &network.generation_memberships[&fixture.topic][&2];
        assert!(
            members.get(&fixture.users[D].0).is_none(),
            "変更前の構成で新しい世代を作った"
        );
    }

    #[test]
    fn 降格の通知前に作った世代が遅れて届いても受け付けない() {
        let mut fixture = Fixture::new();
        let mut network = fixture.network();
        let a = fixture.join(&mut network, A);
        network.run().expect("最初の取得が止まらなかった");
        fixture.append(&mut network, a, "本文。");
        assert_eq!(network.queued(a).0, 1, "変更前に世代の記録を作らなかった");

        fixture.roles[B] = Role::Proposer;
        let membership = fixture.membership(&[A, B, C, D]);
        network
            .change_membership(&membership, MembershipChange::Reduced, fixture.topic, &[])
            .expect("役割を下げられなかった");
        assert!(
            network.send_one(a),
            "遅れていた記録を中継へ届けられなかった"
        );
        assert!(
            matches!(
                network.devices[a.0].downlink.front(),
                Some(Incoming::Rejected {
                    reason: RejectReason::StaleMembership,
                    ..
                })
            ),
            "変更前の役割で包んだ世代の記録を受け付けた",
        );
        assert!(
            network.relay.generations(&fixture.topic).is_empty(),
            "古い構成の世代を保存した"
        );
        network
            .receive_one(a)
            .expect("変更前の記録の拒否を処理できなかった");
        network.poll(a);
        network
            .run()
            .expect("メンバー構成を待つあいだに送信が止まらなかった");
        assert!(
            network.relay.generations(&fixture.topic).is_empty(),
            "新しい構成に切り替わる前にまた世代を作った",
        );
        network
            .notify_membership(fixture.topic, &[a])
            .expect("遅れた通知を届けられなかった");
        network.run().expect("更新した構成で同期が止まった");
        assert_eq!(
            network.client(a).pending(&fixture.page),
            0,
            "役割を下げたあとの構成で編集を送れなかった"
        );
        assert_eq!(
            network.generation_memberships[&fixture.topic][&1]
                .get(&fixture.users[B].0)
                .expect("Bの配布時の役割が無い")
                .role,
            Role::Proposer,
            "旧構成のまま世代を配った",
        );
    }

    #[test]
    fn 過去の世代の作成者が外れた後でも残った端末は履歴を取得できる() {
        let fixture = Fixture::new();
        let mut network = fixture.network();
        let a = fixture.join(&mut network, A);
        fixture.append(&mut network, a, "変更前。");
        network.settle().expect("変更前の同期が止まらなかった");
        network.disconnect(a);
        let membership = fixture.membership(&[B, C, D]);
        network
            .change_membership(&membership, MembershipChange::Reduced, fixture.topic, &[])
            .expect("世代の作成者を外せなかった");

        let b = fixture.join_with(&mut network, B, membership.clone());
        network.run().expect("履歴の取得が止まらなかった");
        assert_eq!(
            fixture.text(&network, b),
            "変更前。",
            "配布時の構成で過去の鍵を開けなかった"
        );
        fixture.append(&mut network, b, "変更後。");
        network.run().expect("変更後の送信が止まらなかった");
        let d = fixture.join_with(&mut network, D, membership);
        network.run().expect("閲覧側の履歴の取得が止まらなかった");
        assert_eq!(
            fixture.text(&network, d),
            "変更前。変更後。",
            "新旧の世代を通して履歴を取得できなかった"
        );
    }

    #[test]
    fn 配送件数を上りと下りで1件ずつ数える() {
        let fixture = Fixture::new();
        let mut network = fixture.network();
        let a = fixture.join(&mut network, A);

        assert!(
            matches!(network.run_with_limit(1), Err(NetworkError::NotQuiet)),
            "上限の時点で配送待ちのメッセージを報告しなかった",
        );
        assert_eq!(network.queued(a), (1, 1), "上限が1件なのに応答まで配送した");
        network
            .run_with_limit(3)
            .expect("ちょうど上限までで配送を終えられなかった");
        assert_eq!(
            network.queued(a),
            (0, 0),
            "配送した件数と列に残った件数が合わない"
        );
    }

    #[test]
    fn 同期の結果をデバッグ表示しても本文を出さない() {
        let fixture = Fixture::new();
        let step = Step {
            changes: vec![PageChange {
                page: fixture.page,
                replacements: vec![Replacement {
                    range: 0..0,
                    text: "テスト用の本文".to_owned(),
                }],
            }],
            ..Step::default()
        };
        let message = format!("{step:?}");
        assert!(
            !message.contains("テスト用の本文"),
            "同期のデバッグ表示に本文が出た"
        );
        assert!(
            message.contains("replacement_count: 1"),
            "デバッグ表示に変更の件数が無い"
        );
    }

    #[test]
    fn 同時に2台が新しい世代を作ったら_先に届いた世代に揃い_更新を暗号化し直して送る() {
        let fixture = Fixture::new();
        let mut network = fixture.network();
        let a = fixture.join(&mut network, A);
        let b = fixture.join(&mut network, B);
        let c = fixture.join(&mut network, C);
        fixture.append(&mut network, a, "外す前。");
        network.settle().expect("同期が止まらなかった");

        network
            .change_membership(
                &fixture.membership(&[A, B, C]),
                MembershipChange::Reduced,
                fixture.topic,
                &[a, b, c],
            )
            .expect("メンバー構成の変更を知らせられなかった");
        // AとBが、互いの世代を知る前に書き、それぞれ世代2の記録を作る。Aの記録が先に届く。
        fixture.append(&mut network, a, "Aの更新。");
        fixture.append(&mut network, b, "Bの更新。");
        network.send_all(a);
        network.send_all(b);
        network.settle().expect("同期が止まらなかった");

        let generations = network.relay().generations(&fixture.topic);
        assert_eq!(generations.len(), 2, "世代の記録が2つではない");
        let accepted =
            GenerationRecord::parse(generations[1].clone()).expect("世代の記録を読めなかった");
        assert_eq!(
            accepted.header().generation,
            2,
            "受け付けた世代の番号が2ではない"
        );
        assert_eq!(
            network.client(b).current_generation(),
            Some(2),
            "Bが受け付けられた世代に揃わなかった"
        );
        assert_eq!(
            network.client(b).pending(&fixture.page),
            0,
            "Bの更新が受け付けられなかった"
        );

        let expected = fixture.text(&network, a);
        for device in [b, c] {
            assert_eq!(
                fixture.text(&network, device),
                expected,
                "端末の文書が食い違った"
            );
        }
        for text in ["Aの更新。", "Bの更新。"] {
            assert!(
                expected.contains(text),
                "{text}の書き込みが失われた: {expected}"
            );
        }
    }

    fn page_text(network: &Network, device: DeviceId, page: SyncPageId) -> String {
        network
            .client(device)
            .document(&page)
            .expect("ページを開いていない")
            .text()
            .to_owned()
    }

    /// 2つ目の方式を読め、中継の最低限の版が2になったら2つ目の方式で書く、版2のクライアント。
    fn second_suite_policy() -> SuitePolicy {
        SuitePolicy {
            client_version: 2,
            readable: vec![Suite::CURRENT, Suite::TestSecond],
            writable: vec![(Suite::CURRENT, 1), (Suite::TestSecond, 2)],
        }
    }

    #[test]
    fn まとめたスナップショットから後から参加した端末が同じ文書になり_版の中身を取り出せる() {
        let fixture = Fixture::new();
        let mut network = fixture.network();
        let a = fixture.join(&mut network, A);
        fixture.append(&mut network, a, "一版目。");
        network
            .create_version(a, fixture.page)
            .expect("版を付けられなかった");
        fixture.append(&mut network, a, "二版目の追記。");
        network.settle().expect("同期が止まらなかった");
        let b = fixture.join(&mut network, B);
        network.settle().expect("同期が止まらなかった");

        network
            .compact(a, fixture.page)
            .expect("まとめられなかった");
        network.run().expect("同期が止まらなかった");
        let stored = network.relay().fetch(&fixture.body_log(fixture.page), None);
        assert!(stored.snapshot.is_some(), "中継がまとめを保存していない");
        assert!(
            stored.records.is_empty(),
            "中継がまとめた範囲のレコードを消していない: {}件",
            stored.records.len(),
        );

        // まとめたあとに書いた更新は、まとめの後ろに続く。
        fixture.append(&mut network, b, "まとめのあと。");
        let d = fixture.join(&mut network, D);
        network.settle().expect("同期が止まらなかった");

        let expected = fixture.text(&network, a);
        for device in [b, d] {
            assert_eq!(
                fixture.text(&network, device),
                expected,
                "端末の文書が食い違った"
            );
        }
        assert!(
            expected.ends_with("まとめのあと。"),
            "まとめのあとの更新が無い: {expected}"
        );
        let document = network
            .client(d)
            .document(&fixture.page)
            .expect("ページを開いていない");
        let versions = network.client(d).versions(&fixture.page);
        assert_eq!(versions.len(), 1, "まとめから受け取った版の数が異なる");
        let content = versions[0]
            .content(document)
            .expect("まとめたあとに版の中身を取り出せなかった");
        assert_eq!(content.body, "一版目。", "版の時点の本文が異なる");
    }

    #[test]
    fn まとめより前から分かれていた端末も_つなぎ直してまとめた文書に合わさる() {
        let fixture = Fixture::new();
        let mut network = fixture.network();
        let a = fixture.join(&mut network, A);
        let b = fixture.join(&mut network, B);
        fixture.append(&mut network, a, "最初。");
        network.settle().expect("同期が止まらなかった");

        network.disconnect(b);
        fixture.append(&mut network, b, "Bが切れているあいだ。");
        fixture.append(&mut network, a, "Aの追記。");
        network.settle().expect("同期が止まらなかった");
        network
            .compact(a, fixture.page)
            .expect("まとめられなかった");
        network.run().expect("同期が止まらなかった");

        network.connect(b);
        network.settle().expect("同期が止まらなかった");
        let expected = fixture.text(&network, a);
        assert_eq!(
            fixture.text(&network, b),
            expected,
            "つなぎ直したBの文書がAと食い違った"
        );
        for text in ["最初。", "Bが切れているあいだ。", "Aの追記。"] {
            assert!(
                expected.contains(text),
                "{text}の書き込みが失われた: {expected}"
            );
        }
        assert_eq!(
            network.client(b).pending(&fixture.page),
            0,
            "Bの更新が受け付けられなかった"
        );
    }

    #[test]
    fn まとめる前提がそろわないページはまとめない() {
        let fixture = Fixture::new();
        let mut network = fixture.network();
        let a = fixture.join(&mut network, A);
        let d = fixture.join(&mut network, D);
        assert!(
            matches!(
                network.compact(a, fixture.page),
                Err(NetworkError::Client(nonoto_core::Error::CompactionNotReady))
            ),
            "まだ取得していないページをまとめた",
        );
        fixture.append(&mut network, a, "本文。");
        network.settle().expect("同期が止まらなかった");
        fixture.append(&mut network, a, "送る前。");
        assert!(
            matches!(
                network.compact(a, fixture.page),
                Err(NetworkError::Client(nonoto_core::Error::CompactionNotReady))
            ),
            "送っていない更新があるページをまとめた",
        );
        assert!(
            matches!(
                network.compact(d, fixture.page),
                Err(NetworkError::Client(nonoto_core::Error::NotEditor))
            ),
            "閲覧の役割の端末がまとめた",
        );
    }

    #[test]
    fn 受付と取得が前後しても_取得済みの範囲だけをまとめる() {
        let fixture = Fixture::new();
        let mut network = fixture.network();
        let a = fixture.join(&mut network, A);
        fixture.append(&mut network, a, "最初。");
        network.settle().expect("最初の同期が止まらなかった");

        fixture.append(&mut network, a, "受付が先。");
        network.run().expect("編集を送れなかった");
        assert_eq!(
            network.client(a).pending(&fixture.page),
            0,
            "受付が届いても送信待ちが残った"
        );
        assert!(
            matches!(
                network.compact(a, fixture.page),
                Err(NetworkError::Client(nonoto_core::Error::CompactionNotReady))
            ),
            "受付済みで未取得の更新をまとめに入れた"
        );
        network.poll(a);
        network.run().expect("受け付けられた更新を取得できなかった");

        fixture.append(&mut network, a, "取得が先。");
        network.poll(a);
        network.send_all(a);
        network.reverse_downlink(a);
        assert!(
            network.receive_one(a).expect("取得結果を処理できなかった"),
            "取得結果が届かなかった"
        );
        assert_eq!(
            network.client(a).pending(&fixture.page),
            1,
            "受付の前に送信待ちが消えた"
        );
        assert!(
            matches!(
                network.compact(a, fixture.page),
                Err(NetworkError::Client(nonoto_core::Error::CompactionNotReady))
            ),
            "受付に対応づけていない編集をまとめに入れた"
        );
        network.run().expect("受付が届かなかった");
        network
            .compact(a, fixture.page)
            .expect("まとめられなかった");
        network.settle().expect("まとめを送れなかった");

        let stored = network.relay().fetch(&fixture.body_log(fixture.page), None);
        assert_eq!(stored.snapshot_info.map(|info| info.last_sequence), Some(3));
        assert_eq!(stored.records, Vec::new());
        let d = fixture.join(&mut network, D);
        network.run().expect("まとめを取得できなかった");
        assert_eq!(
            fixture.text(&network, d),
            "最初。受付が先。取得が先。",
            "取得したまとめの本文が異なる"
        );
    }

    #[test]
    fn 空の取得応答が後着しても_新しい世代が届くまでまとめない() {
        let fixture = Fixture::new();
        let mut network = fixture.network();
        let a = fixture.join(&mut network, A);
        let b = fixture.join(&mut network, B);
        fixture.append(&mut network, a, "元の本文。");
        network.settle().expect("最初の同期が止まらなかった");

        network.poll(b);
        network.send_all(b);
        let earlier = network.devices[b.0]
            .downlink
            .pop_front()
            .expect("先の取得結果が無い");
        network
            .change_membership(
                &fixture.membership(&[A, B, C]),
                MembershipChange::Reduced,
                fixture.topic,
                &[a],
            )
            .expect("世代を閉じられなかった");
        fixture.append(&mut network, a, "次の世代。");
        network.run().expect("新しい世代で送れなかった");

        network.poll(b);
        network.send_all(b);
        network.devices[b.0].downlink.push_back(earlier);
        assert!(
            network
                .receive_one(b)
                .expect("新しい取得結果を処理できなかった"),
            "新しい取得結果が届かなかった"
        );
        assert!(
            network
                .receive_one(b)
                .expect("古い取得結果を処理できなかった"),
            "古い取得結果が届かなかった"
        );
        assert!(
            matches!(
                network.compact(b, fixture.page),
                Err(NetworkError::Client(nonoto_core::Error::CompactionNotReady))
            ),
            "古い応答で取得を終えたと判断した"
        );

        network.run().expect("世代の取得を再開できなかった");
        assert_eq!(
            fixture.text(&network, b),
            "元の本文。次の世代。",
            "世代の到着後に取得を再開できなかった"
        );
        network
            .compact(b, fixture.page)
            .expect("取得後にまとめられなかった");
    }

    #[test]
    fn 通常の編集を足さずに新しい世代を作ってまとめ直せる() {
        let fixture = Fixture::new();
        let mut network = fixture.network();
        let a = fixture.join_policy(&mut network, A, second_suite_policy());
        fixture.append(&mut network, a, "最初の世代。");
        network.settle().expect("最初の同期が止まらなかった");

        network
            .change_membership(
                &fixture.membership(&[A, B, C]),
                MembershipChange::Reduced,
                fixture.topic,
                &[a],
            )
            .expect("最初の世代を閉じられなかった");
        network.relay_mut().set_minimum_client_version(2);
        network
            .rewrite_old_pages(a)
            .expect("まとめ直しを始められなかった");
        network.run().expect("世代を用意してまとめ直せなかった");

        assert_eq!(
            network.client(a).current_generation(),
            Some(2),
            "まとめ用の新しい世代を作れなかった"
        );
        assert_eq!(
            network
                .relay()
                .pages_with_suite(&fixture.topic, Suite::CURRENT),
            0
        );
        assert_eq!(
            network
                .relay()
                .pages_with_suite(&fixture.topic, Suite::TestSecond),
            1
        );
        let b = fixture.join_as(
            &mut network,
            B,
            fixture.membership(&[A, B, C]),
            second_suite_policy(),
        );
        network.run().expect("まとめを取得できなかった");
        assert_eq!(
            fixture.text(&network, b),
            "最初の世代。",
            "移行後に新しい世代でまとめを取得できなかった"
        );
    }

    #[test]
    fn 旧方式の廃止と行き違った編集は新方式で送り直す() {
        let fixture = Fixture::new();
        let mut network = fixture.network();
        let a = fixture.join_policy(&mut network, A, second_suite_policy());
        let b = fixture.join_policy(&mut network, B, second_suite_policy());
        fixture.append(&mut network, a, "切り替え前。");
        network.settle().expect("最初の同期が止まらなかった");
        fixture.append(&mut network, b, "配送待ち。");
        let in_flight = network.devices[b.0]
            .uplink
            .pop_front()
            .expect("旧方式の送信が無い");

        network.relay_mut().set_minimum_client_version(2);
        network
            .rewrite_old_pages(a)
            .expect("まとめ直しを始められなかった");
        network.run().expect("まとめ直しを送れなかった");
        network
            .relay_mut()
            .retire_suite(&fixture.topic, Suite::CURRENT)
            .expect("旧方式を廃止できなかった");
        network.devices[b.0].uplink.push_back(in_flight);
        network
            .run()
            .expect("再取得と新方式での送信が止まらなかった");
        network.settle().expect("送り直した編集が届かなかった");

        assert_eq!(network.client(b).pending(&fixture.page), 0);
        assert!(
            !network.client(b).is_blocked(&fixture.page),
            "旧方式の拒否でページの送信を止めた"
        );
        assert_eq!(
            fixture.text(&network, a),
            "切り替え前。配送待ち。",
            "送り直した編集がAに届かなかった"
        );
        assert_eq!(
            fixture.text(&network, b),
            fixture.text(&network, a),
            "方式の切り替え後に本文が食い違った"
        );
        assert_eq!(
            network
                .relay()
                .pages_with_suite(&fixture.topic, Suite::CURRENT),
            0
        );
    }

    #[test]
    fn 旧方式の廃止と行き違ったまとめは新方式でまとめ直す() {
        let fixture = Fixture::new();
        let mut network = fixture.network();
        let a = fixture.join_policy(&mut network, A, second_suite_policy());
        let b = fixture.join_policy(&mut network, B, second_suite_policy());
        fixture.append(&mut network, a, "切り替え前。");
        network.settle().expect("最初の同期が止まらなかった");
        network
            .compact(b, fixture.page)
            .expect("旧方式でまとめられなかった");
        let in_flight = network.devices[b.0]
            .uplink
            .pop_front()
            .expect("旧方式のまとめが無い");

        network.relay_mut().set_minimum_client_version(2);
        network
            .rewrite_old_pages(a)
            .expect("まとめ直しを始められなかった");
        network.run().expect("まとめ直しを送れなかった");
        network
            .relay_mut()
            .retire_suite(&fixture.topic, Suite::CURRENT)
            .expect("旧方式を廃止できなかった");
        network.devices[b.0].uplink.push_back(in_flight);
        network
            .run()
            .expect("再取得と新方式でのまとめが止まらなかった");

        // 受付番号1はAのまとめ直しで、2が廃止と行き違ったBのまとめを、Bが新方式でまとめ直したもの。
        let stored = network.relay().fetch(&fixture.body_log(fixture.page), None);
        assert_eq!(
            stored.snapshot_info,
            Some(SnapshotInfo {
                revision: 2,
                last_sequence: 1,
                suite: Suite::TestSecond,
            }),
            "廃止と行き違ったまとめを、新方式でまとめ直さなかった"
        );
        assert!(
            !network.client(b).is_blocked(&fixture.page),
            "旧方式の拒否でページの送信を止めた"
        );
    }

    #[test]
    fn 世代の閉鎖と行き違ったまとめは新しい世代でまとめ直す() {
        let fixture = Fixture::new();
        let mut network = fixture.network();
        let a = fixture.join(&mut network, A);
        fixture.append(&mut network, a, "まとめる本文。");
        network.settle().expect("最初の同期が止まらなかった");
        network
            .compact(a, fixture.page)
            .expect("まとめられなかった");
        let in_flight = network.devices[a.0]
            .uplink
            .pop_front()
            .expect("まとめが無い");

        // 世代を閉じる変更は中継にだけ知らせ、まとめが拒まれてから端末に知らせる。
        let reduced = fixture.membership(&[A, B, C]);
        network
            .change_membership(&reduced, MembershipChange::Reduced, fixture.topic, &[])
            .expect("最初の世代を閉じられなかった");
        network.devices[a.0].uplink.push_back(in_flight);
        network
            .run()
            .expect("閉じた世代のまとめの拒否を処理できなかった");
        network
            .notify_membership(fixture.topic, &[a])
            .expect("メンバー構成を知らせられなかった");
        network.run().expect("新しい世代でまとめ直せなかった");

        assert_eq!(
            network.client(a).current_generation(),
            Some(2),
            "まとめ用の新しい世代を作れなかった"
        );
        let stored = network.relay().fetch(&fixture.body_log(fixture.page), None);
        let snapshot = SealedRecord::parse(stored.snapshot.expect("中継にまとめが無い"))
            .expect("まとめの封筒を読めなかった");
        assert_eq!(
            snapshot.header().generation,
            2,
            "閉じた世代と行き違ったまとめを、新しい世代でまとめ直さなかった"
        );
        let b = fixture.join_with(&mut network, B, reduced);
        network.run().expect("まとめを取得できなかった");
        assert_eq!(
            fixture.text(&network, b),
            "まとめる本文。",
            "新しい世代のまとめを取得できなかった"
        );
    }

    #[test]
    fn 旧方式の廃止と行き違った世代の記録は新方式で作り直す() {
        let fixture = Fixture::new();
        let mut network = fixture.network();
        let a = fixture.join_policy(&mut network, A, second_suite_policy());
        network.run().expect("最初の取得が止まらなかった");
        fixture.append(&mut network, a, "最初の世代。");
        let in_flight = network.devices[a.0]
            .uplink
            .pop_front()
            .expect("旧方式の世代の記録が無い");

        // ページの暗号文がまだ無いため、旧方式はすぐに受け付けなくできる。
        network.relay_mut().set_minimum_client_version(2);
        network
            .relay_mut()
            .retire_suite(&fixture.topic, Suite::CURRENT)
            .expect("旧方式を廃止できなかった");
        network.devices[a.0].uplink.push_back(in_flight);
        network.run().expect("新方式で世代を作り直せなかった");

        assert_eq!(
            network.client(a).pending(&fixture.page),
            0,
            "世代を作り直して編集を送れなかった"
        );
        let mut generations = network.relay().generations(&fixture.topic);
        assert_eq!(generations.len(), 1, "中継が受け付けた世代の数が異なる");
        let generation =
            GenerationRecord::parse(generations.remove(0)).expect("世代の記録を読めなかった");
        assert_eq!(
            generation.header().suite,
            Suite::TestSecond,
            "世代の記録を新方式で作り直さなかった"
        );
        assert_eq!(
            network
                .relay()
                .pages_with_suite(&fixture.topic, Suite::TestSecond),
            1
        );
    }

    #[test]
    fn まとめ直しの途中で書いた編集の受付が届いたら_取得し直してまとめる() {
        let fixture = Fixture::new();
        let mut network = fixture.network();
        let a = fixture.join_policy(&mut network, A, second_suite_policy());
        fixture.append(&mut network, a, "最初。");
        network.settle().expect("最初の同期が止まらなかった");

        network.relay_mut().set_minimum_client_version(2);
        network
            .rewrite_old_pages(a)
            .expect("まとめ直しを始められなかった");
        // 取得し終える前に書いた編集は、まとめより先に送られ、取得した最後の順番より後で受け付けられる。
        fixture.append(&mut network, a, "まとめ直しの途中。");
        // `poll` を挟まず、受付のあとの取得し直しだけでまとめ直しを終える。
        network.run().expect("まとめ直しが止まらなかった");

        let stored = network.relay().fetch(&fixture.body_log(fixture.page), None);
        assert_eq!(
            stored
                .snapshot_info
                .map(|info| (info.last_sequence, info.suite)),
            Some((2, Suite::TestSecond)),
            "受付のあとに取得し直さず、まとめ直しが終わらなかった"
        );
        assert_eq!(stored.records, Vec::new(), "まとめた範囲のレコードが残った");
        assert_eq!(
            network
                .relay()
                .pages_with_suite(&fixture.topic, Suite::CURRENT),
            0
        );
    }

    fn assert_migrated_documents(
        fixture: &Fixture,
        network: &Network,
        other_page: SyncPageId,
        a: DeviceId,
        receivers: [DeviceId; 2],
    ) {
        for page in [fixture.page, other_page] {
            let expected = page_text(network, a, page);
            for device in receivers {
                assert_eq!(
                    page_text(network, device, page),
                    expected,
                    "移したあとの文書が食い違った"
                );
            }
        }
        let expected = fixture.text(network, a);
        for text in ["1つ目の方式。", "Bの追記。", "2つ目の方式。"] {
            assert!(
                expected.contains(text),
                "{text}の書き込みが失われた: {expected}"
            );
        }
        assert!(
            !expected.contains("古い端末。"),
            "拒んだ古い端末の書き込みが文書に入った: {expected}"
        );
    }

    #[test]
    fn 方式を切り替えて全ページを移し_中継が古い暗号文を数えて受け付けなくする() {
        let fixture = Fixture::new();
        let other_page = SyncPageId::generate().expect("ページのIDを作れなかった");
        let mut network = fixture.network();
        let a = fixture.join_policy(&mut network, A, second_suite_policy());
        let old = fixture.join(&mut network, A);
        let b = fixture.join_policy(&mut network, B, second_suite_policy());
        for device in [old, b] {
            network
                .open_page(device, other_page)
                .expect("ページを開けなかった");
        }

        // 1. 2つ目の方式を読める端末も、最低限の版が上がるまでは1つ目の方式で書く。
        fixture.append(&mut network, a, "1つ目の方式。");
        network
            .edit(b, other_page, 0..0, "別のページ。")
            .expect("手元の編集ができなかった");
        network.settle().expect("同期が止まらなかった");
        let relay = network.relay();
        assert_eq!(relay.pages_with_suite(&fixture.topic, Suite::CURRENT), 2);
        assert_eq!(relay.pages_with_suite(&fixture.topic, Suite::TestSecond), 0);

        // 2. 最低限の版を上げると、古いクライアントの書き込みを拒み、読み出しは続ける。
        network.relay_mut().set_minimum_client_version(2);
        fixture.append(&mut network, old, "古い端末。");
        network.run().expect("同期が止まらなかった");
        assert!(
            network.client(old).is_blocked(&fixture.page),
            "最低限の版より古い端末の書き込みを、中継が拒まなかった",
        );
        fixture.append(&mut network, b, "Bの追記。");
        network.settle().expect("同期が止まらなかった");
        assert!(
            fixture.text(&network, old).contains("Bの追記。"),
            "最低限の版より古い端末が、読み出しを続けられなかった",
        );
        assert_eq!(
            network.client(a).write_suite(),
            Some(Suite::TestSecond),
            "最低限の版が上がっても、2つ目の方式で書き始めない",
        );

        // 3. 2つ目の方式で書き、編集の端末がトピックの全ページを順にまとめ直す。
        assert!(
            matches!(
                network
                    .relay_mut()
                    .retire_suite(&fixture.topic, Suite::CURRENT),
                Err(Rejection::OldCiphertextRemains(2))
            ),
            "古い暗号文が残るのに、古い方式を受け付けなくした",
        );
        fixture.append(&mut network, a, "2つ目の方式。");
        network.settle().expect("同期が止まらなかった");
        assert!(
            network.client(a).document(&other_page).is_none(),
            "移行する端末は別ページを開いていない"
        );
        network.rewrite_old_pages(a).expect("まとめ直せなかった");
        network.settle().expect("同期が止まらなかった");
        assert!(
            !network.client(b).needs_rewrite(&other_page),
            "別の端末のまとめで保存中の方式が更新されなかった"
        );

        // 4. 古い暗号文が残るページが0になったら、古い方式を受け付けなくする。
        let relay = network.relay();
        assert_eq!(relay.pages_with_suite(&fixture.topic, Suite::CURRENT), 0);
        assert_eq!(relay.pages_with_suite(&fixture.topic, Suite::TestSecond), 2);
        network
            .relay_mut()
            .retire_suite(&fixture.topic, Suite::CURRENT)
            .expect("古い暗号文が無いのに、古い方式を受け付けなくできなかった");
        let old_record =
            fixture.signed_update(&network, A, fixture.page, Branch::Body, "古い方式。");
        assert!(
            matches!(
                network
                    .relay_mut()
                    .submit(&fixture.users[A].0, 2, old_record),
                Err(Rejection::RetiredSuite(Suite::CURRENT))
            ),
            "受け付けなくした方式のレコードを受け付けた",
        );

        let d = fixture.join_policy(&mut network, D, second_suite_policy());
        network
            .open_page(d, other_page)
            .expect("ページを開けなかった");
        network.settle().expect("同期が止まらなかった");
        assert_migrated_documents(&fixture, &network, other_page, a, [b, d]);
    }

    #[test]
    fn 廃止した方式では_中継の最低限の版が古くても書かない() {
        let fixture = Fixture::new();
        let mut network = fixture.network();
        let a = fixture.join_policy(
            &mut network,
            A,
            SuitePolicy {
                client_version: 3,
                readable: vec![Suite::CURRENT, Suite::TestSecond],
                writable: vec![(Suite::TestSecond, 2)],
            },
        );
        fixture.append(&mut network, a, "廃止した方式で書かない。");
        network.settle().expect("同期が止まらなかった");
        assert_eq!(network.client(a).write_suite(), None, "書ける方式がある");
        assert_eq!(
            network.client(a).pending(&fixture.page),
            1,
            "書ける方式が無いのに、更新が送信待ちから減った"
        );
        assert!(
            network.relay().generations(&fixture.topic).is_empty()
                && network
                    .relay()
                    .pages_with_suite(&fixture.topic, Suite::CURRENT)
                    == 0,
            "廃止した方式で、世代の記録かレコードを書いた",
        );

        network.relay_mut().set_minimum_client_version(2);
        network.settle().expect("同期が止まらなかった");
        assert_eq!(
            network.client(a).pending(&fixture.page),
            0,
            "最低限の版が上がっても、2つ目の方式で送らなかった"
        );
        assert_eq!(
            network
                .relay()
                .pages_with_suite(&fixture.topic, Suite::TestSecond),
            1
        );
        assert_eq!(
            network
                .relay()
                .pages_with_suite(&fixture.topic, Suite::CURRENT),
            0
        );
    }

    #[test]
    fn 知らない方式のレコードを受け取った端末は_そのページに当てず_まとめ直しも書き戻しもしない() {
        let fixture = Fixture::new();
        let mut network = fixture.network();
        // 2つ目の方式で書く新しい端末と、1つ目の方式だけを読める古い端末。
        let a = fixture.join_policy(
            &mut network,
            A,
            SuitePolicy {
                client_version: 2,
                readable: vec![Suite::CURRENT, Suite::TestSecond],
                writable: vec![(Suite::TestSecond, 1)],
            },
        );
        let b = fixture.join(&mut network, B);
        fixture.append(&mut network, a, "2つ目の方式。");
        network.settle().expect("同期が止まらなかった");

        assert!(
            network.client(b).is_stalled(&fixture.page),
            "知らない方式のレコードで、ページへの適用を止めなかった",
        );
        assert_eq!(
            fixture.text(&network, b),
            "",
            "知らない方式のレコードを文書に当てた"
        );
        assert!(
            matches!(
                network.compact(b, fixture.page),
                Err(NetworkError::Client(nonoto_core::Error::PageStalled))
            ),
            "適用を止めたページをまとめ直した",
        );
        let before = network
            .relay()
            .fetch(&fixture.body_log(fixture.page), None)
            .records
            .len();
        fixture.append(&mut network, b, "書き戻さない。");
        network
            .rewrite_old_pages(b)
            .expect("まとめ直しの確かめが失敗した");
        network.settle().expect("同期が止まらなかった");
        assert_eq!(
            network
                .relay()
                .fetch(&fixture.body_log(fixture.page), None)
                .records
                .len(),
            before,
            "適用を止めたページに、手元の編集を書き戻した",
        );
        assert_eq!(
            fixture.text(&network, a),
            "2つ目の方式。",
            "古い端末の書き込みが、新しい端末に届いた"
        );
    }
}
