//! GPUIのデスクトップアプリの画面。

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::component::list::{List, ListDelegate, ListEvent, ListItem, ListState};
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme, IndexPath, ThemeStyled, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, Entity, FocusHandle, Focusable, KeyBinding, Role, SharedString, Subscription,
    TestSupportExt, Window, actions, div, px, relative,
};
use nonoto_core::{NewPage, Page, PageId, PageStore, PageSummary, PageUpdate, Pages};

actions!(nonoto, [SavePage]);

/// キーボードの割り当てのコンテキスト。
const KEY_CONTEXT: &str = "PageBrowser";

/// アプリのキーボードの割り当てを登録する。`gpui_kit::init` のあとに1回呼ぶ。
pub fn init(cx: &mut App) {
    // 保存は、macOSではCommand、それ以外ではControlとの組み合わせにする。
    let save = if cfg!(target_os = "macos") {
        "cmd-s"
    } else {
        "ctrl-s"
    };
    cx.bind_keys([KeyBinding::new(save, SavePage, Some(KEY_CONTEXT))]);
}

/// ページの一覧と、選んだページの本文を並べて表示する画面。
///
/// テストと支援技術 (VoiceOverなど) から要素を読めるよう、要素にIDと役割と名前を付ける。
pub struct PageBrowser<S> {
    pages: Pages<S>,
    list: Entity<ListState<PageList>>,
    /// 一覧で選んでいるページ。本文を読めなかったページでも選択を保ち、キーボードでその先へ進めるようにする。
    selected_id: Option<PageId>,
    /// 選んでいて、本文を読めたページ。
    shown: Option<ShownPage>,
    error: Option<SharedString>,
    /// 編集しているページの入力。編集していないときは `None`。
    editing: Option<Editing>,
    title_input: Entity<InputState>,
    body_input: Entity<TextareaState>,
    _selection: Subscription,
}

/// 編集を始めたときの内容。変更の通知を出さないIMEの確定も保存できるよう、終了時に入力と比べる。
struct Editing {
    id: PageId,
    original_title: SharedString,
    original_body: SharedString,
}

/// 本文を読めたページ。
///
/// Markdownの部品は前の描画と同じ割り当ての本文なら比較を省くため、渡す本文を選んだときに1回だけ作って持つ。
struct ShownPage {
    page: Page,
    body: SharedString,
}

struct PageList {
    summaries: Vec<PageSummary>,
    selected: Option<IndexPath>,
}

impl ListDelegate for PageList {
    type Item = ListItem;

    fn items_count(&self, _: usize, _: &App) -> usize {
        self.summaries.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) -> Option<ListItem> {
        let summary = self.summaries.get(ix.row)?;
        let title = display_title(&summary.title);
        Some(
            ListItem::new(("page", ix.row))
                .accessibility_label(title.clone())
                .aria_selected(self.selected == Some(ix))
                .h(px(42.))
                .rounded(px(4.))
                .child(div().text_ellipsis().child(title)),
        )
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) {
        self.selected = ix;
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        div()
    }
}

impl<S: PageStore + 'static> PageBrowser<S> {
    /// ページの一覧を読み、先頭のページを選んだ状態で作る。
    pub fn new(pages: Pages<S>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let list = cx.new(|cx| {
            ListState::new(
                PageList {
                    summaries: Vec::new(),
                    selected: None,
                },
                window,
                cx,
            )
        });
        let selection = cx.subscribe_in(
            &list,
            window,
            |this: &mut Self, list, event, window, cx| match event {
                ListEvent::Select(ix) | ListEvent::Confirm(ix) => {
                    let id = list
                        .read(cx)
                        .delegate()
                        .summaries
                        .get(ix.row)
                        .map(|summary| summary.id);
                    if let Some(id) = id {
                        this.select(id, window, cx);
                    }
                }
                ListEvent::Cancel => this.sync_selection(window, cx),
            },
        );
        let focus = list.focus_handle(cx).tab_stop(true);
        cx.on_focus_in(&focus, window, |_, _, cx| cx.notify())
            .detach();
        cx.on_focus_out(&focus, window, |_, _, _, cx| cx.notify())
            .detach();
        // ウィンドウを閉じるときも、別のページへ移るときと同じく、変えた内容を保存してから閉じる。
        // 保存できなければ閉じず、エラーを表示する。
        let this = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            this.update(cx, |this, cx| this.finish_editing(window, cx))
                .unwrap_or(true)
        });
        let title_input = cx.new(|cx| InputState::new(window, cx).placeholder("タイトル"));
        let body_input =
            cx.new(|cx| TextareaState::new(window, cx).placeholder("本文をMarkdownで書く"));
        let mut browser = Self {
            pages,
            list,
            selected_id: None,
            shown: None,
            error: None,
            editing: None,
            title_input,
            body_input,
            _selection: selection,
        };
        browser.reload(window, cx);
        focus.focus(window, cx);
        browser
    }

    /// ページの一覧を読み直す。選んでいたページが無くなっていれば、先頭のページを選ぶ。
    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let summaries = match self.pages.list() {
            Ok(summaries) => {
                self.error = None;
                summaries
            }
            Err(err) => {
                self.error = Some(error_message(&err).into());
                cx.notify();
                return;
            }
        };
        let selected = self
            .selected_id
            .filter(|id| summaries.iter().any(|summary| summary.id == *id))
            .or_else(|| summaries.first().map(|summary| summary.id));
        self.list
            .update(cx, |list, _| list.delegate_mut().summaries = summaries);
        match selected {
            Some(id) => self.select(id, window, cx),
            None => {
                self.selected_id = None;
                self.shown = None;
                self.sync_selection(window, cx);
                cx.notify();
            }
        }
    }

    /// ページを選び、本文を読む。本文を読めなくても、一覧の選択はこのページに置く。
    ///
    /// 編集中に別のページを選んだときは、変えた内容を保存してから移る。保存できなければ移らない。
    pub fn select(&mut self, id: PageId, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .editing
            .as_ref()
            .is_some_and(|editing| editing.id != id)
            && !self.finish_editing(window, cx)
        {
            self.sync_selection(window, cx);
            cx.notify();
            return;
        }
        self.selected_id = Some(id);
        match self.pages.get(id) {
            Ok(page) => {
                let body = SharedString::from(page.body.clone());
                self.shown = Some(ShownPage { page, body });
                self.error = None;
            }
            Err(err) => {
                self.shown = None;
                self.error = Some(error_message(&err).into());
            }
        }
        self.sync_selection(window, cx);
        cx.notify();
    }

    /// 選んでいて、本文を読めたページ。
    pub fn selected(&self) -> Option<&Page> {
        self.shown.as_ref().map(|shown| &shown.page)
    }

    /// ページを編集しているか。
    pub fn is_editing(&self) -> bool {
        self.editing.is_some()
    }

    /// タイトルの入力。テストで、入力の部品に直接テキストを送るときに使う。
    pub fn title_input(&self) -> &Entity<InputState> {
        &self.title_input
    }

    /// 本文の入力。テストで、IMEの変換中のテキストを送るときに使う。
    pub fn body_input(&self) -> &Entity<TextareaState> {
        &self.body_input
    }

    /// 空のページを作り、そのページの編集を始める。
    pub fn new_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.finish_editing(window, cx) {
            return;
        }
        match self.pages.create(NewPage::default()) {
            Ok(page) => {
                self.selected_id = Some(page.id);
                self.reload(window, cx);
                self.edit(window, cx);
                self.title_input.focus_handle(cx).focus(window, cx);
            }
            Err(err) => {
                self.error = Some(error_message(&err).into());
                cx.notify();
            }
        }
    }

    /// 表示しているページの編集を始める。
    pub fn edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ShownPage { page, body }) = &self.shown else {
            return;
        };
        let editing = Editing {
            id: page.id,
            original_title: page.title.clone().into(),
            original_body: body.clone(),
        };
        self.title_input.update(cx, |input, cx| {
            input.set_value(editing.original_title.clone(), window, cx)
        });
        self.body_input.update(cx, |input, cx| {
            input.set_value(editing.original_body.clone(), window, cx)
        });
        self.editing = Some(editing);
        self.body_input.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    /// 編集した内容を保存し、表示に戻る。保存できなければ編集を続ける。
    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.editing.as_ref().map(|editing| editing.id) else {
            return;
        };
        let update = PageUpdate {
            title: Some(self.title_input.read(cx).value().to_string()),
            body: Some(self.body_input.read(cx).value().to_string()),
        };
        match self.pages.update(id, update) {
            Ok(_) => {
                self.editing = None;
                self.selected_id = Some(id);
                self.reload(window, cx);
                self.list.focus_handle(cx).focus(window, cx);
            }
            Err(err) => {
                self.error = Some(error_message(&err).into());
                cx.notify();
            }
        }
    }

    /// 編集した内容を捨て、表示に戻る。
    ///
    /// 編集中に出るエラーは保存の失敗だけで、捨てた編集についてのものなので、一緒に消す。
    pub fn cancel_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing = None;
        self.error = None;
        self.list.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    /// 編集をやめる。変えた内容があれば保存し、保存できたか変えていなければ `true` を返す。
    fn finish_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match &self.editing {
            None => true,
            Some(editing)
                if self.title_input.read(cx).value() == editing.original_title
                    && self.body_input.read(cx).value() == editing.original_body =>
            {
                self.editing = None;
                true
            }
            Some(_) => {
                self.save(window, cx);
                self.editing.is_none()
            }
        }
    }

    fn on_save(&mut self, _: &SavePage, window: &mut Window, cx: &mut Context<Self>) {
        self.save(window, cx);
    }

    fn sync_selection(&self, window: &mut Window, cx: &mut Context<Self>) {
        let selected_id = self.selected_id;
        self.list.update(cx, |list, cx| {
            let ix = list
                .delegate()
                .summaries
                .iter()
                .position(|summary| Some(summary.id) == selected_id)
                .map(IndexPath::new);
            list.set_selected_index(ix, window, cx);
            cx.notify();
        });
    }

    fn render_list(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("page-list")
            .test_support()
            .role(Role::Group)
            .aria_label("ページの一覧")
            .w(px(260.))
            .h_full()
            .flex_none()
            .p_2()
            .border_r_1()
            .border_color(cx.theme().border)
            .when(self.list.focus_handle(cx).is_focused(window), |pane| {
                pane.focus_ring_style(window, cx)
            })
            .child(List::new(&self.list))
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<S> {
        let buttons = if self.editing.is_some() {
            vec![
                Button::new("save")
                    .primary()
                    .label("保存")
                    .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                Button::new("cancel")
                    .outline()
                    .label("キャンセル")
                    .on_click(cx.listener(|this, _, window, cx| this.cancel_editing(window, cx))),
            ]
        } else {
            let mut buttons = vec![
                Button::new("new-page")
                    .outline()
                    .label("新しいページ")
                    .on_click(cx.listener(|this, _, window, cx| this.new_page(window, cx))),
            ];
            if self.shown.is_some() {
                buttons.push(
                    Button::new("edit")
                        .primary()
                        .label("編集")
                        .on_click(cx.listener(|this, _, window, cx| this.edit(window, cx))),
                );
            }
            buttons
        };
        h_flex()
            .id("toolbar")
            .test_support()
            .role(Role::Toolbar)
            .aria_label("ページの操作")
            .gap_2()
            .justify_end()
            .children(buttons)
    }

    fn render_editor(&self) -> impl IntoElement {
        v_flex()
            .id("editor")
            .flex_1()
            .min_h_0()
            .gap_4()
            .child(
                Input::new(&self.title_input)
                    .id("title-input")
                    .aria_label("タイトル"),
            )
            .child(
                div().flex_1().min_h_0().child(
                    Textarea::new(&self.body_input)
                        .aria_label("本文")
                        .h(relative(1.)),
                ),
            )
    }

    fn render_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let toolbar = self.render_toolbar(cx);
        let theme = cx.theme();
        let content = match (&self.shown, &self.error) {
            (Some(ShownPage { page, body }), _) => {
                let title = display_title(&page.title);
                v_flex()
                    .gap_4()
                    .child(
                        div()
                            .id("page-title")
                            .test_support()
                            .role(Role::Heading)
                            .aria_label(title.clone())
                            .text_2xl()
                            .font_weight(gpui_kit::FontWeight::BOLD)
                            .child(title),
                    )
                    .child(TextView::markdown("page-body", body.clone()).selectable(true))
            }
            // 読み込みに失敗したときは、ページが無いとは言えないため、エラーだけを出す。
            (None, Some(_)) => v_flex(),
            (None, None) => v_flex().child(
                div()
                    .id("empty")
                    .test_support()
                    .aria_label("ページがありません")
                    .text_color(theme.muted_foreground)
                    .child("ページがありません"),
            ),
        };
        let editing = self.editing.is_some();
        v_flex()
            .id("page")
            .flex_1()
            // 長い段落を領域の幅で折り返すよう、内容の幅より狭く縮められるようにする。
            .min_w_0()
            .h_full()
            .p_6()
            .gap_4()
            // 編集中は本文の入力が自分でスクロールするため、ページの側ではスクロールしない。
            .when(!editing, |pane| pane.overflow_y_scroll())
            .child(toolbar)
            .when_some(self.error.clone(), |pane, error| {
                pane.child(
                    div()
                        .id("error")
                        .test_support()
                        .role(Role::Alert)
                        .aria_label(error.clone())
                        .text_color(theme.danger)
                        .child(error),
                )
            })
            .map(|pane| {
                if editing {
                    pane.child(self.render_editor())
                } else {
                    pane.child(content)
                }
            })
    }
}

impl<S: PageStore + 'static> Focusable for PageBrowser<S> {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.list.focus_handle(cx)
    }
}

impl<S: PageStore + 'static> Render for PageBrowser<S> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(Self::on_save))
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_list(window, cx))
            .child(self.render_page(cx))
    }
}

/// 空のタイトルも一覧で選べるよう、代わりの文言を出す。
fn display_title(title: &str) -> SharedString {
    if title.is_empty() {
        SharedString::from("無題のページ")
    } else {
        SharedString::from(title.to_owned())
    }
}

/// エラーの文言。原因もたどって1行にまとめる。
///
/// 起動時に出すエラーと画面に出すエラーを同じ形にするため、どちらもこの関数で作る。
pub fn error_message(err: &nonoto_core::Error) -> String {
    let mut message = err.to_string();
    let mut source = std::error::Error::source(err);
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}
