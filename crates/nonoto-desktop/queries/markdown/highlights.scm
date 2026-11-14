; Markdownのブロックの記法の装飾。
; 名前は gpui-component のハイライトの名前から選び、色と書体はテーマの側 (markdown_highlight.rs) で決める。
; 記法の記号は punctuation、本文より薄く見せる範囲は comment にする。

(atx_heading (inline) @title)
(setext_heading (paragraph) @title)

(block_quote) @comment

[
  (indented_code_block)
  (fenced_code_block)
] @text.literal

[
  (atx_h1_marker)
  (atx_h2_marker)
  (atx_h3_marker)
  (atx_h4_marker)
  (atx_h5_marker)
  (atx_h6_marker)
  (setext_h1_underline)
  (setext_h2_underline)
  (list_marker_plus)
  (list_marker_minus)
  (list_marker_star)
  (list_marker_dot)
  (list_marker_parenthesis)
  (task_list_marker_checked)
  (task_list_marker_unchecked)
  (thematic_break)
  (block_quote_marker)
  (fenced_code_block_delimiter)
  (info_string)
  (backslash_escape)
] @punctuation

((link_reference_definition (link_label) @link_text)
  (#set! highlight.allow-overlap))
(link_reference_definition (link_destination) @link_uri)
(link_reference_definition (link_title) @link_uri)
(link_label ["[" "]"] @punctuation)
(link_reference_definition ":" @punctuation)

((html_block) @comment
  (#match? @comment "^<!--"))
