; Markdownのインラインの記法の装飾。名前の決め方はブロックの記法と同じ。

((code_span) @text.code.span
  (#set! highlight.allow-overlap))

((emphasis) @emphasis
  (#set! highlight.allow-overlap))

((strong_emphasis) @emphasis.strong
  (#set! highlight.allow-overlap))

[
  (link_text)
  (image_description)
  (uri_autolink)
  (email_autolink)
] @link_text

((link_label) @link_text
  (#set! highlight.allow-overlap))

[
  (link_destination)
  (link_title)
] @link_uri

[
  (emphasis_delimiter)
  (code_span_delimiter)
  (backslash_escape)
] @punctuation

(inline_link ["[" "]" "(" ")"] @punctuation)
(image ["!" "[" "]" "(" ")"] @punctuation)
(full_reference_link ["[" "]"] @punctuation)
(collapsed_reference_link ["[" "]"] @punctuation)
(shortcut_link ["[" "]"] @punctuation)
(link_label ["[" "]"] @punctuation)

((html_tag) @comment
  (#match? @comment "^<!--"))
