; 段落や見出しの中の記法は、インラインの文法で装飾する。
; コードブロックの中は言語ごとに色付けしないため、言語の注入を書かない。

((inline) @injection.content
  (#set! injection.language "markdown_inline")
  (#set! injection.combined))
