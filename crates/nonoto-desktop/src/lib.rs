//! GPUIのデスクトップアプリの画面。

pub mod markdown_highlight;
mod wikilink_completion;
mod wikilink_decoration;
mod wikilink_view;

use std::fmt;
use std::ops::Range;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{
    Editor, EditorState, Enter, IndentInline, Input, InputEvent, InputState, MoveDown, MoveUp,
    OutdentInline, TextDecorationCollection,
};
use gpui_kit::component::list::{List, ListDelegate, ListEvent, ListItem, ListState};
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme, IndexPath, Theme, ThemeStyled, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    Action, App, ClickEvent, Context, Entity, EntityInputHandler, FocusHandle, Focusable,
    KeyBinding, MouseButton, Role, SharedString, Subscription, TestSupportExt, Window, actions,
    div, px, relative,
};
use nonoto_core::{NewPage, Page, PageId, PageStore, PageSummary, PageUpdate, Pages, TextEdit};

actions!(nonoto, [SavePage]);

/// キーボードの割り当てのコンテキスト。
const KEY_CONTEXT: &str = "PageBrowser";

/// コアが編集不要と判断したとき、部品の既定の処理へ渡すか。
#[derive(Clone, Copy)]
enum NoEditBehavior {
    Delegate,
    Consume,
}

/// アプリのキーボードの割り当てと、本文の装飾を登録する。`gpui_kit::init` のあとに1回呼ぶ。
pub fn init(cx: &mut App) {
    markdown_highlight::register_languages();
    markdown_highlight::apply_to_theme(cx);
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
/// デバッグ表示ではページの内容を伏せ、選択と編集の有無を表示する。
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
    body_input: Entity<EditorState>,
    /// 本文の入力のWikiリンクの装飾。
    wikilinks: TextDecorationCollection,
    _subscriptions: Vec<Subscription>,
}

impl<S> fmt::Debug for PageBrowser<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PageBrowser")
            .field("page_selected", &self.selected_id.is_some())
            .field("editing", &self.editing.is_some())
            .finish_non_exhaustive()
    }
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
    /// 描画に渡す本文。Wikiリンクを、アプリの中だけのスキームのリンクに置き換えたもの。
    rendered: SharedString,
    /// 描画に渡す本文のWikiリンクの番号ごとのリンク先。
    wikilink_targets: Vec<PageId>,
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
        let body_input = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .language(markdown_highlight::LANGUAGE)
                .placeholder("本文をMarkdownで書く");
            // 今のWikinoのエディタに合わせ、行番号を出して折り返す。折りたたみは使わない。
            state.set_line_number(true, window, cx);
            state.set_soft_wrap(true, window, cx);
            state.set_folding(false, window, cx);
            state
        });
        let wikilinks = body_input.update(cx, |state, cx| {
            state.create_decorations_collection(Vec::new(), cx)
        });
        // `[[` の後ろで、タイトルにキーワードを含むページを候補に出す。
        // 探せなかったときは、入力を妨げないよう候補を出さない。
        let this = cx.weak_entity();
        let search: Rc<wikilink_completion::SearchTitles> = Rc::new(move |keyword, cx| {
            this.read_with(cx, |browser, _| {
                browser
                    .pages
                    .search_by_title(keyword, wikilink_completion::CANDIDATE_LIMIT)
            })
            .ok()
            .and_then(Result::ok)
            .map_or_default(|summaries| {
                summaries.into_iter().map(|summary| summary.title).collect()
            })
        });
        body_input.update(cx, |state, _| {
            let lsp = state.lsp_mut();
            lsp.completion_provider = Some(Rc::new(wikilink_completion::WikilinkCompletion::new(
                search,
            )));
            lsp.set_completion_highlighter(Rc::new(wikilink_completion::highlight));
        });
        // 本文を変えるたびに、Wikiリンクを解析し直して装飾を付け直す。
        // IMEの変換中は通知が無いが、装飾の範囲は部品が編集に合わせてずらす。
        let body_changed = cx.subscribe(&body_input, |this: &mut Self, _, event, cx| {
            if let InputEvent::Change = event {
                this.refresh_wikilinks(cx);
            }
        });
        // 装飾の色はテーマのハイライトから取るため、ライトとダークを切り替えたら付け直す。
        let theme_changed = cx.observe_global::<Theme>(|this: &mut Self, cx| {
            this.refresh_wikilinks(cx);
        });
        let mut browser = Self {
            pages,
            list,
            selected_id: None,
            shown: None,
            error: None,
            editing: None,
            title_input,
            body_input,
            wikilinks,
            _subscriptions: vec![selection, body_changed, theme_changed],
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
        if let Some(id) = selected {
            self.select(id, window, cx);
        } else {
            self.selected_id = None;
            self.shown = None;
            self.sync_selection(window, cx);
            cx.notify();
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
                // リンク先は表示するときに決める。探せなかったリンク先は、無いものとして扱う。
                let rendered = wikilink_view::render(&page.body, |title| {
                    self.pages
                        .find_by_title(title)
                        .ok()
                        .flatten()
                        .map(|summary| summary.id)
                });
                self.shown = Some(ShownPage {
                    body: SharedString::from(page.body.clone()),
                    rendered: rendered.markdown.into(),
                    wikilink_targets: rendered.targets,
                    page,
                });
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

    /// 表示している本文として描画に渡しているMarkdown。テストで、Wikiリンクの置き換えを確かめるときに使う。
    pub fn rendered_body(&self) -> Option<&str> {
        self.shown.as_ref().map(|shown| shown.rendered.as_ref())
    }

    /// 表示している本文の、番号のWikiリンクのリンク先のページへ移る。
    fn open_wikilink(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let target = self
            .shown
            .as_ref()
            .and_then(|shown| shown.wikilink_targets.get(index))
            .copied();
        if let Some(id) = target {
            self.select(id, window, cx);
        }
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
    pub fn body_input(&self) -> &Entity<EditorState> {
        &self.body_input
    }

    /// 本文の入力でWikiリンクとして装飾している範囲。テストで、装飾が本文の位置と合っているかを確かめるときに使う。
    pub fn wikilink_decoration_ranges(&self, cx: &App) -> Vec<Range<usize>> {
        self.wikilinks.get_ranges(cx)
    }

    /// 本文の入力のWikiリンクを解析し直し、装飾を付け直す。
    fn refresh_wikilinks(&self, cx: &mut App) {
        let body = self.body_input.read(cx).value();
        let links = nonoto_core::find_wikilinks(&body);
        let theme = &cx.theme().highlight_theme;
        let link = theme.style("link_text").unwrap_or_default();
        let mark = theme.style("punctuation").unwrap_or_default();
        self.wikilinks
            .set(wikilink_decoration::decorations(&links, link, mark), cx);
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
        let Some(ShownPage { page, body, .. }) = &self.shown else {
            return;
        };
        let editing = Editing {
            id: page.id,
            original_title: page.title.clone().into(),
            original_body: body.clone(),
        };
        self.title_input.update(cx, |input, cx| {
            input.set_value(editing.original_title.clone(), window, cx);
        });
        self.body_input.update(cx, |input, cx| {
            input.set_value(editing.original_body.clone(), window, cx);
        });
        // 本文を入れ替えたときは変更の通知が出ないため、ここで装飾を付け直す。
        self.refresh_wikilinks(cx);
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

    /// Enterで、補完の候補を出していれば候補を選び、リストの項目ならリストを続ける。
    /// Shiftなどと組み合わせたEnterは、部品に委ねる。
    /// 候補の表示中は、部品の既定動作でShift-Enterも候補を選ぶ。
    fn on_enter(&mut self, action: &Enter, window: &mut Window, cx: &mut Context<Self>) {
        if action.shift || action.secondary {
            return;
        }
        if self.route_to_completion(action, window, cx) {
            return;
        }
        self.edit_body(window, cx, NoEditBehavior::Delegate, |text, selection| {
            selection
                .is_empty()
                .then(|| nonoto_core::continue_list(text, selection.start))
                .flatten()
        });
    }

    /// 補完の候補を出しているときの上下で、候補の選択を動かす。
    fn on_move_up(&mut self, action: &MoveUp, window: &mut Window, cx: &mut Context<Self>) {
        self.route_to_completion(action, window, cx);
    }

    fn on_move_down(&mut self, action: &MoveDown, window: &mut Window, cx: &mut Context<Self>) {
        self.route_to_completion(action, window, cx);
    }

    /// 補完の候補を出しているときのキー操作を、部品の候補に処理させる。候補が処理したら `true` を返す。
    ///
    /// 部品の候補は、処理した操作の伝搬を止めない。
    /// 候補を表示して画面が落ち着いた状態では同じ操作がもう一度届き、
    /// Enterで選んだあとに改行やリストの継続が入り、上下では選択が2つ動く。
    /// キャプチャの段階で候補に渡してから伝搬を止め、1回のキーで1回だけ処理させる。
    fn route_to_completion(
        &mut self,
        action: &dyn Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.editing.is_none() || !self.body_input.focus_handle(cx).is_focused(window) {
            return false;
        }
        let routed = self.body_input.update(cx, |state, cx| {
            // IMEの変換中のキーは、変換の確定などを壊さないよう、部品に任せる。
            if state.marked_text_range(window, cx).is_some() || !state.completion_menu_state().open
            {
                return false;
            }
            state.route_overlay_action(action.boxed_clone(), window, cx)
        });
        if routed {
            cx.stop_propagation();
        }
        routed
    }

    fn on_indent(&mut self, _: &IndentInline, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_body(window, cx, NoEditBehavior::Consume, |text, selection| {
            Some(nonoto_core::indent(text, selection))
        });
    }

    fn on_outdent(&mut self, _: &OutdentInline, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_body(window, cx, NoEditBehavior::Consume, nonoto_core::outdent);
    }

    /// 本文の入力に、コアの編集のロジックが返した編集を当てる。
    ///
    /// 部品より先にキー操作のアクションを受け取って呼ぶ。
    /// 編集を当てたときは、部品の既定の処理 (改行やインデント) に渡さない。
    /// 編集不要のときは `no_edit` に従う。Shift-Tabは部品の別の規則で字下げを削らせない。
    /// IMEの変換中は、変換の確定などのキー操作を壊さないよう、何もせずに部品に渡す。
    fn edit_body(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        no_edit: NoEditBehavior,
        edit: impl FnOnce(&str, Range<usize>) -> Option<TextEdit>,
    ) {
        if self.editing.is_none() || !self.body_input.focus_handle(cx).is_focused(window) {
            return;
        }
        let handled = self.body_input.update(cx, |state, cx| {
            if state.marked_text_range(window, cx).is_some() {
                return false;
            }
            let text = state.value();
            let selection = state.selected_range();
            let Some(edit) = edit(&text, selection.clone()) else {
                return matches!(no_edit, NoEditBehavior::Consume);
            };
            if edit.range == selection {
                // 現在の選択の置換では、入力と分けて取り消せる部品のAPIを使う。
                state.replace(edit.text, window, cx);
            } else {
                // 先に選択を変えると、取り消しで元の選択へ戻れないため、範囲を指定して置き換える。
                // IMEのトレイトの範囲はUTF-16の位置なので、コアのUTF-8のバイト位置を変換する。
                let start = text[..edit.range.start].encode_utf16().count();
                let end = start + text[edit.range].encode_utf16().count();
                // この置き換えは利用者が打った文字として扱われ、補完の判定に置き換えた文字列が渡る。
                // 文字を打っていない操作で補完の候補を開かないよう、置き換えの間だけ補完を外す。
                let completion = state.lsp_mut().completion_provider.take();
                state.replace_text_in_range(Some(start..end), &edit.text, window, cx);
                state.lsp_mut().completion_provider = completion;
            }
            state.set_selected_range(edit.selection, cx);
            true
        });
        if handled {
            cx.stop_propagation();
        }
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
                    Editor::new(&self.body_input)
                        .aria_label("本文")
                        .h(relative(1.)),
                ),
            )
    }

    fn render_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let toolbar = self.render_toolbar(cx);
        let theme = cx.theme();
        let content = match (&self.shown, &self.error) {
            (Some(ShownPage { page, rendered, .. }), _) => {
                let title = display_title(&page.title);
                let this = cx.weak_entity();
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
                    .child(
                        div().id("page-body").test_support().child(
                            TextView::markdown("page-body-text", rendered.clone())
                                .selectable(true)
                                .on_link_click(move |url, event, window, cx| {
                                    if !opens_link(event) {
                                        return;
                                    }
                                    match wikilink_view::target_index(url) {
                                        Some(index) => {
                                            this.update(cx, |this, cx| {
                                                this.open_wikilink(index, window, cx);
                                            })
                                            .ok();
                                        }
                                        None => cx.open_url(url),
                                    }
                                }),
                        ),
                    )
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
            .when(
                !editing,
                gpui_kit::StatefulInteractiveElement::overflow_y_scroll,
            )
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
            // 本文の入力の部品より先に受け取るため、キャプチャの段階で受け取る。
            .capture_action(cx.listener(Self::on_enter))
            .capture_action(cx.listener(Self::on_move_up))
            .capture_action(cx.listener(Self::on_move_down))
            .capture_action(cx.listener(Self::on_indent))
            .capture_action(cx.listener(Self::on_outdent))
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_list(window, cx))
            .child(self.render_page(cx))
    }
}

/// 押したリンクを開くか。
///
/// 描画の部品は、リンクを押したときの処理を渡すと、リンクを開く既定の処理をしない。
/// Wikiリンク以外のリンクを既定と同じく開くため、部品が開く押し方 (左か中のボタン・キーボード・長押しでないタッチ) に揃える。
fn opens_link(event: &ClickEvent) -> bool {
    match event {
        ClickEvent::Mouse(click) => {
            matches!(click.up.button, MouseButton::Left | MouseButton::Middle)
        }
        ClickEvent::Keyboard(_) => true,
        ClickEvent::Touch(click) => !click.long_press,
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
