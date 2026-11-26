//! トピックのメンバーと、利用者の鍵。
//!
//! メンバー構成の履歴の形式はまだ持たず、呼び出し側 (テスト) が組んだメンバー構成を [`Membership`] で渡す。
//! 利用者の鍵は保存しない。

use std::collections::HashSet;
use std::fmt;

use crate::crypto::{Suite, UserSigningKey, UserVerifyingKey, WrapPublicKey, WrapSecretKey};
use crate::error::Error;
use crate::record::random_id;

random_id! {
    /// トピックのメンバーのID。世代の記録で、包みの宛先として封筒に置く。
    MemberId
}

/// トピックの中の役割 (ADR #0009)。後ろのものほど多くの鍵を持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    /// トピックの鍵だけを持ち、本文と編集提案を読む。
    Viewer,
    /// 閲覧に加えて提案の書き込みの鍵を持ち、編集提案の枝に書く。
    Proposer,
    /// 提案に加えて本文の書き込みの鍵を持ち、本文に書く・新しい世代を作る。
    Editor,
}

impl Role {
    /// 世代の記録の包みに入れる値。
    pub(crate) fn value(self) -> u8 {
        match self {
            Self::Viewer => 1,
            Self::Proposer => 2,
            Self::Editor => 3,
        }
    }

    pub(crate) fn from_value(value: u8) -> Result<Self, Error> {
        match value {
            1 => Ok(Self::Viewer),
            2 => Ok(Self::Proposer),
            3 => Ok(Self::Editor),
            _ => Err(Error::MalformedRecord),
        }
    }
}

/// 利用者の鍵の組 (鍵を包むための鍵と、署名の鍵)。利用者のアカウントごとに持ち、端末の間で共有する。
///
/// ログに出さないよう、`Debug` では秘密鍵を表示しない。
#[derive(Clone)]
pub struct UserKeys {
    wrap: WrapSecretKey,
    signing: UserSigningKey,
    public: UserPublicKeys,
}

impl UserKeys {
    /// 方式に合わせて、乱数から新しい利用者の鍵の組を作る。
    pub fn generate(suite: Suite) -> Result<Self, Error> {
        let (wrap, wrap_public) = suite.generate_wrap_keys()?;
        let signing = UserSigningKey::generate()?;
        let public = UserPublicKeys {
            wrap: wrap_public,
            signing: signing.verifying_key(),
        };
        Ok(Self {
            wrap,
            signing,
            public,
        })
    }

    /// 利用者の公開鍵を返す。メンバー構成に入れて、ほかのメンバーに渡す。
    pub fn public_keys(&self) -> &UserPublicKeys {
        &self.public
    }

    pub(crate) fn wrap_secret(&self) -> &WrapSecretKey {
        &self.wrap
    }

    pub(crate) fn signing(&self) -> &UserSigningKey {
        &self.signing
    }
}

impl fmt::Debug for UserKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UserKeys")
            .field("public", &self.public)
            .finish_non_exhaustive()
    }
}

/// 利用者の公開鍵の組。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserPublicKeys {
    /// 鍵を包むための公開鍵。
    pub wrap: WrapPublicKey,
    /// 署名の公開鍵。
    pub signing: UserVerifyingKey,
}

/// トピックのメンバー。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub id: MemberId,
    pub role: Role,
    pub keys: UserPublicKeys,
}

/// トピックのメンバー構成。端末と中継に、同じものを渡す。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Membership {
    members: Vec<Member>,
}

impl Membership {
    /// メンバーの一覧から作る。
    ///
    /// IDが重複していれば [`Error::DuplicateMember`]、異なるメンバーが同じ利用者の
    /// 公開鍵を持つときは [`Error::DuplicateMemberKey`] を返す。
    pub fn new(members: Vec<Member>) -> Result<Self, Error> {
        let mut ids = HashSet::with_capacity(members.len());
        let mut wrap_keys = HashSet::with_capacity(members.len());
        let mut signing_keys = HashSet::with_capacity(members.len());
        for member in &members {
            if !ids.insert(member.id) {
                return Err(Error::DuplicateMember);
            }
            if !wrap_keys.insert(member.keys.wrap.as_bytes())
                || !signing_keys.insert(member.keys.signing.canonical_bytes())
            {
                return Err(Error::DuplicateMemberKey);
            }
        }
        Ok(Self { members })
    }

    /// IDのメンバーを返す。メンバーでなければ `None` を返す。
    pub fn get(&self, id: &MemberId) -> Option<&Member> {
        self.members.iter().find(|member| member.id == *id)
    }

    /// トピックの全メンバーを登録順に返す。
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Member> {
        self.members.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(role: Role) -> Member {
        let keys = UserKeys::generate(Suite::CURRENT).expect("利用者の鍵を作れなかった");
        Member {
            id: MemberId::generate().expect("メンバーのIDを作れなかった"),
            role,
            keys: keys.public_keys().clone(),
        }
    }

    #[test]
    fn 同じidのメンバーを2人入れたメンバー構成を拒む() {
        let first = member(Role::Editor);
        let mut second = member(Role::Viewer);
        second.id = first.id;

        assert!(
            matches!(
                Membership::new(vec![first, second]),
                Err(Error::DuplicateMember)
            ),
            "同じIDのメンバーを2人入れたメンバー構成を受け付けた",
        );
    }

    #[test]
    fn 異なるメンバーの受取用公開鍵と署名用公開鍵の重複を拒む() {
        let editor = member(Role::Editor);
        let viewer = member(Role::Viewer);
        for keys in [
            UserPublicKeys {
                wrap: editor.keys.wrap.clone(),
                signing: viewer.keys.signing,
            },
            UserPublicKeys {
                wrap: viewer.keys.wrap.clone(),
                signing: editor.keys.signing,
            },
        ] {
            let viewer = Member {
                keys,
                ..viewer.clone()
            };
            assert!(
                matches!(
                    Membership::new(vec![editor.clone(), viewer]),
                    Err(Error::DuplicateMemberKey)
                ),
                "異なる役割のメンバーに同じ利用者の公開鍵を登録できた"
            );
        }
    }

    #[test]
    fn 役割の値から役割を求め直せる() {
        for role in [Role::Viewer, Role::Proposer, Role::Editor] {
            assert_eq!(
                Role::from_value(role.value()).expect("役割の値から役割を求められなかった"),
                role,
                "役割の値から別の役割が求まった",
            );
        }
        assert!(Role::from_value(0).is_err(), "知らない役割の値を受け付けた");
    }

    #[test]
    fn 利用者の鍵のデバッグ表示に秘密鍵を出さない() {
        let keys = UserKeys::generate(Suite::CURRENT).expect("利用者の鍵を作れなかった");
        let shown = format!("{keys:?}");

        assert!(
            !shown.contains("WrapSecretKey") && !shown.contains("UserSigningKey"),
            "利用者の鍵のデバッグ表示に秘密鍵の欄を出した: {shown}",
        );
    }
}
