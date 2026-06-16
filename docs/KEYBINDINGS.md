# onda — Keybindings reference

Complete list of built-in keybindings and `:` commands, generated from the source
(`onda-modal` keymap + the `onda` binary's mode handlers). Notation: `<C-x>` = Ctrl-x,
`<space>` = the leader, `<CR>` = Enter, `<Esc>` = Escape. Most normal-mode keys accept
a leading **count** (e.g. `3x`, `2dd`, `5j`).

The keymap is static (compile-time, ADR-106). A future command palette (`F1`) will list
these interactively — until then, this file is the reference.

---

## Normal mode

### Motions
| Key | Motion |
|---|---|
| `h` `l` | left / right (clamped to line) |
| `j` `k` | down / up (sticky goal column) |
| `w` `b` `e` | word forward / back / end |
| `W` `B` `E` | WORD (whitespace-delimited) forward / back / end |
| `0` | line start |
| `^` | first non-blank |
| `$` | line end |
| `{` `}` | paragraph back / forward |
| `gg` `G` | document start / end |
| `<C-d>` `<C-u>` | half-page down / up |
| `<PageDown>` `<PageUp>` | half-page down / up |
| `f{c}` `t{c}` | find / till char forward |
| `F{c}` `T{c}` | find / till char backward |

### Editing
| Key | Action |
|---|---|
| `i` `a` | insert before / after cursor |
| `I` `A` | insert at line start / end |
| `o` `O` | open line below / above (insert) |
| `x` | delete char(s) under cursor |
| `D` `C` | delete / change to end of line |
| `p` `P` | paste after / before |
| `J` | join line below |
| `r{c}` | replace char under cursor with `{c}` |
| `.` | repeat last change (operators + insert) |
| `u` `<C-r>` | undo / redo |
| `g-` `g+` | undo-tree older / newer state |

### Operators (`{op}{motion}` or `{op}{text-object}`)
`d` delete · `c` change · `y` yank. Double the operator for line-wise (`dd`, `cc`, `yy`).
Counts apply (`2dd`, `3dw`).

| Example | Effect |
|---|---|
| `dw` `de` `d$` | delete to next word / word-end / line-end |
| `dj` `dk` `dG` `dgg` | delete whole lines (line-wise motion) |
| `cw` | change word (acts like `ce` on a non-blank) |
| `yy` `dd` `cc` | yank / delete / change line |
| `d{textobj}` | operate on a text object (below) |

### Text objects (after an operator, or in visual mode)
Prefix `i` = inner, `a` = around/outer.

| Object | Keys |
|---|---|
| word / WORD | `iw` `aw` · `iW` `aW` |
| parentheses | `i(` `i)` `ib` · `a(` … |
| brackets | `i[` `i]` · `a[` … |
| braces | `i{` `i}` `iB` · `a{` … |
| double / single quote / backtick | `i"` `i'` `` i` `` · `a"` … |
| paragraph | `ip` `ap` |
| function (tree-sitter) | `if` `af` |
| class (tree-sitter) | `ic` `ac` |
| argument (tree-sitter) | `ia` `aa` |

### Search
| Key | Action |
|---|---|
| `/` `?` | search forward / backward (opens the input line) |
| `n` `N` | next / previous match |
| `*` `#` | search word under cursor forward / backward |

### Marks & macros & registers
| Key | Action |
|---|---|
| `m{c}` | set mark `{c}` |
| `` `{c} `` | jump to mark `{c}` (exact position) |
| `'{c}` | jump to mark `{c}` (line) |
| `q{c}` … `q` | record macro into register `{c}`, stop with `q` |
| `@{c}` | play macro `{c}` |
| `@@` | replay last macro |
| `"{c}` | use register `{c}` for the next yank/paste/delete |

### Windows, pickers, shell
| Key | Action |
|---|---|
| `<C-w>` | focus next window |
| `<C-o>` `<C-i>` | jump list older / newer (`<C-i>` may arrive as Tab) |
| `<C-v>` | enter visual-block mode |
| `<space>f` | fuzzy file picker |
| `<space>b` | buffer picker |
| `<space>e` | open + focus the IDE sidebar |
| `<space>p` | command palette (fuzzy) |
| `<F1>` | keybinding reference (searchable) |

### Debugger (DAP) function keys
| Key | Action |
|---|---|
| `<F9>` | toggle breakpoint on the current line |
| `<F5>` | continue |
| `<F10>` | step over |
| `<F11>` | step in |
| `<F12>` | step out |

### Mode entry
`v` visual · `V` visual-line · `<C-v>` visual-block · `:` command line.

---

## Visual / visual-line / visual-block mode
| Key | Action |
|---|---|
| motions | extend the selection |
| `d` `c` `y` | delete / change / yank the selection (line-wise in visual-line) |
| `o` | swap anchor / head (move the other end) |
| `i{obj}` `a{obj}` | select a text object |
| `<Esc>` | return to normal mode |

---

## Insert mode
| Key | Action |
|---|---|
| any char | insert at cursor |
| `<CR>` | newline (cursor to start of new line) |
| `<Backspace>` | delete char before cursor (joins lines at column 0) |
| `<Delete>` | delete char at cursor |
| `<Esc>` | leave insert mode (cursor moves left one, vim-style) |

---

## Command-line mode (`:` and `/` `?`)
| Key | Action |
|---|---|
| type | edit the command / search pattern |
| `<Tab>` `<S-Tab>` | cycle command-name / file-path completion (`:` only) |
| `<CR>` | run the command / accept completion |
| `<Backspace>` | delete (empties → leaves the line) |
| `<Esc>` | cancel (first press dismisses an open completion) |

---

## Pickers (file / buffer / …)
| Key | Action |
|---|---|
| type | filter (fuzzy) |
| `j` / `<Down>`, `k` / `<Up>` | move selection |
| `<CR>` | open the selection |
| `<Backspace>` | delete a filter char |
| `<Esc>` | close the picker |

---

## IDE sidebar (when focused — `<space>e`)
| Key | Action |
|---|---|
| `<Tab>` / `<S-Tab>` | next / previous view |
| `1`–`5` | jump to Explorer / Search / Source Control / Run / Agent |
| `<` `>` (or `H` `L`) | shrink / grow the sidebar |
| `<Esc>` | return to the editor (keep the sidebar open) |
| `q` | close the sidebar |

### Explorer view (file tree)
| Key | Action |
|---|---|
| `j` / `<Down>`, `k` / `<Up>` | move selection |
| `l` / `<CR>` | expand a directory, or open a file (focus → editor) |
| `h` | collapse the directory, or jump to its parent |
| `a` / `A` | create a new file / directory (prompts for a name) |
| `r` | rename the selected entry |
| `d` | delete the selected entry (`y` to confirm) |
| `R` | refresh the tree |

In a create/rename prompt: type the name, `<CR>` confirms, `<Esc>` cancels.

### Source Control view (git)
| Key | Action |
|---|---|
| `j` / `<Down>`, `k` / `<Up>` | move selection through changed files |
| `a` | stage the selected file (`git add`) |
| `u` | unstage the selected file (`git reset HEAD`) |
| `c` | commit (prompts for a message; empty cancels) |
| `R` | refresh the status |

Each row shows a two-char `git status` badge (e.g. `M `, ` M`, `??`) and the path.
In the commit prompt: type the message, `<CR>` commits, `<Esc>` cancels.

---

## Previews (images & PDF)
Opening an image (`png` `jpg` `jpeg` `gif` `bmp` `webp` `ico`) or a `pdf` — via the
file picker, the explorer, or `:e <path>` — shows a **read-only preview** instead of
loading binary bytes into a text buffer:

| Terminal | Behaviour |
|---|---|
| kitty graphics / iTerm2 (kitty, Ghostty, WezTerm, iTerm2, …) | the image is drawn inline |
| any other terminal | a metadata card (name, format, dimensions, size) |

PDFs always show the metadata card (page count when detectable); onda does not bundle a
PDF rasterizer. Preview buffers reject `:w` (they have no text content to save).

---

## Diff review (`:agent-review`)
| Key | Action |
|---|---|
| `j` / `<Down>`, `k` / `<Up>` | next / previous hunk |
| `a` / `r` | accept / reject the current hunk |
| `A` / `R` | accept / reject all hunks |
| `<CR>` / `y` | apply accepted hunks (one undo step) |
| `q` / `<Esc>` | cancel the review |

---

## Terminal mode
| Key | Action |
|---|---|
| any key | forwarded to the PTY |
| `<C-n>` | leave terminal mode (back to normal) |

---

## Mouse
| Action | Effect |
|---|---|
| click in the editor | move the cursor |
| scroll | scroll the buffer |
| click a buffer tab | switch buffer |
| click the activity bar | select that sidebar view + focus it |
| click in the sidebar | focus it (Explorer: select the row) |
| drag the sidebar's right border | resize the sidebar |

---

## Ex-commands (`:`)

| Command | Action |
|---|---|
| `:w` `:q` `:wq` `:x` | write / quit / write-quit |
| `:wqa` `:wqall` | write-quit all |
| `:q!` (`:q` force) | force quit |
| `:e <path>` | edit a file (`<Tab>` completes paths) |
| `:sp` / `:split`, `:vsp` / `:vsplit` `[file]` | horizontal / vertical split |
| `:bn` `:bp` | next / previous buffer |
| `:ls` / `:buffers` | list buffers |
| `:noh` / `:nohlsearch` | clear search highlight |
| `:[%]s/pat/rep/[g]` | substitute (current line or whole file with `%`) |
| `:zz` | center the cursor line |
| `:messages` / `:mes` | message history |
| `:theme [name]` | show / switch theme |
| `:Format` | LSP format the buffer |
| `:GrammarFetch` / `:grammars` | fetch tree-sitter grammars |
| `:terminal` / `:term` | open a terminal pane |
| `:session save/restore [name]` | persist / restore session |
| `:table` / `:csv` | toggle CSV/TSV table view |
| `:fields` | JSONL field schema overlay |
| `:agent [name]` | connect / toggle the agent panel |
| `:agent-review` | review agent-proposed edits |
| `:agent-export` | export the agent transcript |
| `:DapRun` / `:DapStop` | start / stop a debug session |
| `:DapBreakpoint` | toggle a breakpoint (same as `<F9>`) |
| `:DapStack` / `:DapVars` | show call stack / variables |
| `:DapEval <expr>` | evaluate an expression at the stop |

> Plugins and the (future) command palette can register additional commands and
> keybindings; this file covers the built-ins.
