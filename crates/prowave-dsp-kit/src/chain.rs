//! A chain of effects, built from a command line like `gain -6 :0 lowpass 1k 0.7q`
//! (`effects_chain.c` of the original).

use crate::effect::{get_effect_info, Buf, Effect, Report, Stream};
use crate::error::{Error, Result};
use crate::selector::parse_selector_masked;

/// `EC_PARSE_MAX_RDEPTH`
const MAX_PARSE_DEPTH: usize = 512;

/// Effects of the original that the port does not have (they need native libraries or
/// files); as names they still end the argument list of the effect before them.
const UNAVAILABLE: [&str; 2] = ["zita_convolver", "ladspa_host"];

/// Effects the parser makes itself, because they need the effects-file reader.
pub(crate) const PARSER_EFFECTS: [&str; 1] = ["watch"];

/// Whether `name` (the original's command or Pro Wave's name) is one of [`PARSER_EFFECTS`].
fn is_parser_effect(name: &str) -> bool {
    PARSER_EFFECTS.contains(&crate::catalog::command_for(name).unwrap_or(name))
}

/// A word of the command line (`struct ec_token`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Token<'a> {
    Literal(&'a str),
    /// A word that began with `\`: always an argument.
    Escaped(&'a str),
    ChSel(&'a str),
    BlockStart,
    BlockEnd,
    Source(&'a str),
    AllowFail,
    NewThread,
}

/// `ec_lex_word` and `ec_get_token_id`
fn lex(w: &str) -> Token<'_> {
    if let Some(rest) = w.strip_prefix('\\') {
        return Token::Escaped(rest);
    }
    if let Some(rest) = w.strip_prefix(':') {
        Token::ChSel(rest)
    } else if w == "{" {
        Token::BlockStart
    } else if w == "}" {
        Token::BlockEnd
    } else if w.len() > 1 && w.starts_with('@') {
        Token::Source(&w[1..])
    } else if w == "!" {
        Token::AllowFail
    } else if w == "new_thread" {
        Token::NewThread
    } else {
        Token::Literal(w)
    }
}

impl<'a> Token<'a> {
    fn text(&self) -> &'a str {
        match *self {
            Token::Literal(s) | Token::Escaped(s) | Token::ChSel(s) | Token::Source(s) => s,
            Token::BlockStart => "{",
            Token::BlockEnd => "}",
            Token::AllowFail => "!",
            Token::NewThread => "new_thread",
        }
    }
    /// `ec_token_is_keyword`: whether the word ends the argument list of an effect.
    fn is_keyword(&self) -> bool {
        match *self {
            Token::Escaped(_) => false,
            Token::Literal(s) => {
                get_effect_info(s).is_some() || UNAVAILABLE.contains(&s) || is_parser_effect(s)
            }
            _ => true,
        }
    }
}

/// Where `@file` effects files come from. The crate reads no files itself: `read` gets the
/// full path (after `~/` and the `%r %k %c %%` substitutions, see [`full_path`]) and returns
/// the text, so the host decides what may be read (on the web: nothing, or its own store).
pub struct EffectsFiles<'a> {
    /// The directory that relative paths on the command line start from; `None` leaves them
    /// as they are (relative to the working directory, as in the original).
    pub dir: Option<String>,
    /// `$HOME`, for paths that start with `~/`.
    pub home: Option<String>,
    /// Reads the effects file at a path.
    pub read: &'a mut dyn FnMut(&str) -> std::io::Result<String>,
}

/// `construct_full_path`: `path` relative to `dir` (unless absolute or `~/`, which is
/// `home`), with `%r` the sample rate in Hz, `%k` in kHz, `%c` the number of channels and
/// `%%` a `%`.
pub fn full_path(
    dir: Option<&str>,
    path: &str,
    fs: u32,
    channels: usize,
    home: Option<&str>,
) -> String {
    let mut out = String::new();
    let mut p = path;
    if let Some(rest) = p.strip_prefix('~').filter(|r| r.starts_with('/')) {
        p = rest;
        if let Some(h) = home {
            out.push_str(h);
        }
    } else if let Some(d) = dir.filter(|_| !p.starts_with('/')) {
        out.push_str(d);
        out.push('/');
    }
    let mut chars = p.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' || chars.peek().is_none() {
            out.push(c);
            continue;
        }
        match chars.peek().copied() {
            Some('r') => out.push_str(&fs.to_string()),
            Some('k') => out.push_str(&(f64::from(fs) / 1000.0).to_string()),
            Some('c') => out.push_str(&channels.to_string()),
            Some('%') => out.push('%'),
            _ => {
                // not a substitution: the `%` stays and the next character is read as usual
                out.push('%');
                continue;
            }
        }
        chars.next();
    }
    out
}

/// `ec_split_and_lex_string`: the words of an effects file. Words are separated by white
/// space; `#` at the start of a word begins a comment that runs to the end of the line;
/// `"…"` keeps white space and `#` in a word; `\` makes the next character literal (and at
/// the start of a word stays, so that the word is an argument, as on the command line).
pub fn split_words(text: &str) -> Result<Vec<String>> {
    let s = text.as_bytes();
    let n = s.len();
    let (mut sep, mut esc, mut quo) = (true, false, false);
    let (mut i, mut k) = (0usize, 0usize);
    let mut buf: Vec<u8> = Vec::new();
    let mut words = Vec::new();
    loop {
        let c = if k < n { s[k] } else { 0 };
        let mut sp = true;
        let mut done = false;
        if c == b'\\' && !esc {
            esc = true;
            if sep {
                buf.push(c);
            }
        } else if c == b'"' && !esc {
            quo = !quo;
        } else if c == b'#' && !esc && !quo && sep {
            while k < n && s[k] != b'\n' {
                k += 1;
            }
            if k >= n {
                break;
            }
            i = k + 1;
        } else if c == 0 || (!esc && !quo && c.is_ascii_whitespace()) || (!esc && !quo && c == 0x0b)
        {
            if c == 0 {
                if quo {
                    return Err(Error::Message("unterminated quoted string".into()));
                }
                done = true;
            }
            if i != k {
                words.push(String::from_utf8_lossy(&buf).into_owned());
                buf.clear();
                i = k;
            }
            sep = true;
            i += 1;
        } else {
            sp = false;
            buf.push(c);
        }
        if !sp {
            sep = false;
            esc = false;
        }
        k += 1;
        if done {
            break;
        }
    }
    Ok(words)
}

/// What the parser carries through files and blocks: the file reader, if any.
struct ParseCtx<'r, 'a> {
    files: Option<&'r mut EffectsFiles<'a>>,
}

/// `struct ec_parser_state`: the selector and the channel mask of a block.
struct ParseState {
    ch_sel: Vec<bool>,
    ch_mask: Vec<bool>,
    last_ch_sel: Option<String>,
    last_stream_ch: usize,
    allow_fail: bool,
    /// The directory of the effects file being read (relative paths start there).
    dir: Option<String>,
}

impl ParseState {
    /// `ec_parser_state_ch_sel_mask`: the mask is the given one (all channels at the top) and
    /// the selector starts as the whole mask.
    fn new(mask: Option<&[bool]>, n: usize) -> Self {
        let ch_mask = mask.map_or_else(|| vec![true; n], <[bool]>::to_vec);
        Self {
            ch_sel: ch_mask.clone(),
            ch_mask,
            last_ch_sel: None,
            last_stream_ch: n,
            allow_fail: false,
            dir: None,
        }
    }

    /// The mask for a stream that now has `n` channels: added channels are appended to the
    /// mask; with fewer channels the mask keeps its first ones.
    fn update_mask(&mut self, n: usize) {
        let last = self.last_stream_ch;
        let mut tmp = vec![false; n];
        if n > last {
            tmp[..last].copy_from_slice(&self.ch_mask[..last]);
            tmp[last..].fill(true);
        } else {
            let mut nb =
                (self.ch_mask[..last].iter().filter(|b| **b).count() + n) as isize - last as isize;
            for j in 0..n {
                if nb <= 0 {
                    break;
                }
                if self.ch_mask[j] {
                    tmp[j] = true;
                    nb -= 1;
                }
            }
        }
        self.ch_mask = tmp;
    }
}

/// The original's default block size.
pub const DEFAULT_BLOCK_FRAMES: usize = 2048;

/// Effects run one after another over blocks of frames.
pub struct Chain {
    istream: Stream,
    ostream: Stream,
    effects: Vec<Box<dyn Effect>>,
    block_frames: usize,
    buf1: Vec<f64>,
    buf2: Vec<f64>,
    built: bool,
    /// Frames of silence that follow the input to flush the effects (`drain_frames`), and the
    /// number of input frames seen so far.
    drain_frames: usize,
    iframes: usize,
    /// Where `new_thread` started a new part of the chain (`effects_subchain`): the
    /// optimiser does not merge across these.
    barriers: Vec<usize>,
    new_sc: bool,
    oframes: usize,
    /// The `watch` effects of the chain, for the host to reload.
    watches: Vec<crate::watch::WatchHandle>,
}

impl Chain {
    /// An empty chain for `istream`.
    pub fn new(istream: Stream) -> Self {
        Self {
            istream,
            ostream: istream,
            effects: Vec::new(),
            block_frames: DEFAULT_BLOCK_FRAMES,
            buf1: Vec::new(),
            buf2: Vec::new(),
            built: false,
            drain_frames: 0,
            iframes: 0,
            barriers: Vec::new(),
            new_sc: false,
            oframes: 0,
            watches: Vec::new(),
        }
    }

    /// Sets how many frames go through the chain at a time (the original's `-b`).
    pub fn set_block_frames(&mut self, frames: usize) {
        self.block_frames = frames.max(1);
    }

    /// The stream that comes out of the chain.
    pub fn ostream(&self) -> Stream {
        self.ostream
    }

    /// Adds the effect `name` with its arguments, working on the channels of `selector`
    /// (`None`: all of them).
    pub fn push(&mut self, name: &str, args: &[&str], selector: Option<&[bool]>) -> Result<()> {
        let info = get_effect_info(name).ok_or_else(|| Error::UnknownEffect(name.into()))?;
        let mut argv = vec![name];
        argv.extend_from_slice(args);
        let all = vec![true; self.ostream.channels];
        let made = (info.init)(info, &self.ostream, selector.unwrap_or(&all), &argv)?;
        if self.new_sc && !made.is_empty() {
            if !self.effects.is_empty() {
                self.barriers.push(self.effects.len());
            }
            self.new_sc = false;
        }
        for e in made {
            self.ostream = e.ostream();
            self.effects.push(e);
        }
        self.built = false;
        Ok(())
    }

    /// Finishes the chain (`build_effects_chain_finish`): lets each effect prepare, puts in the
    /// `align` effects that keep channels together, and works out how many frames of silence
    /// flush the effects. Called by [`Chain::from_args`]; call it after the last
    /// [`Chain::push`] to hear about errors, otherwise the first [`Chain::run`] does it.
    pub fn build(&mut self) -> Result<()> {
        if self.built || self.effects.is_empty() {
            self.built = true;
            return Ok(());
        }
        // each part of the chain (see `new_thread`) is optimised on its own
        let mut parts: Vec<Vec<Box<dyn Effect>>> = Vec::new();
        let mut rest = std::mem::take(&mut self.effects);
        for &b in self.barriers.iter().rev() {
            parts.push(rest.split_off(b));
        }
        parts.push(rest);
        for part in parts.iter_mut().rev() {
            crate::optimize::optimize(part);
            self.effects.append(part);
        }
        self.barriers.clear();
        for e in self.effects.iter_mut() {
            e.prepare()?;
        }
        crate::align::align_channels(&mut self.effects)?;
        self.drain_frames = crate::align::drain_frames(&self.effects, self.istream, self.ostream);
        self.built = true;
        Ok(())
    }

    /// Builds a chain from command-line words, with the original's syntax (`ec_parse`):
    /// effect names followed by their arguments; `:selector` words that choose the channels
    /// of the effects after them; `{ … }` blocks, inside which the selectors count only the
    /// channels selected where the block starts; `!` before an effect that may fail to
    /// start (it is then left out); and `\word` for an argument that would otherwise be
    /// read as an effect name or a directive. `new_thread` is accepted: the original runs
    /// the rest of the chain in another thread, which changes no sample here; the optimiser
    /// does not merge across it, as in the original. Effects files (`@file`) need
    /// [`Chain::from_args_with_files`].
    pub fn from_args(istream: Stream, argv: &[&str]) -> Result<Self> {
        Self::from_words(istream, argv, None, None)
    }

    /// [`Chain::from_args`] with `@file` effects files read through `files`.
    pub fn from_args_with_files(
        istream: Stream,
        argv: &[&str],
        files: &mut EffectsFiles,
    ) -> Result<Self> {
        let dir = files.dir.clone();
        Self::from_words(istream, argv, Some(files), dir)
    }

    /// The `watch` effects of the chain: the host reloads them when their files change.
    pub fn watches(&self) -> &[crate::watch::WatchHandle] {
        &self.watches
    }

    /// A chain from the text of an effects file (as `@file` would read it); its relative
    /// paths start from `files.dir`.
    pub fn from_effects_text(
        istream: Stream,
        text: &str,
        files: &mut EffectsFiles,
    ) -> Result<Self> {
        let words = split_words(text)?;
        let argv: Vec<&str> = words.iter().map(String::as_str).collect();
        let dir = files.dir.clone();
        Self::from_words(istream, &argv, Some(files), dir)
    }

    fn from_words(
        istream: Stream,
        argv: &[&str],
        files: Option<&mut EffectsFiles>,
        dir: Option<String>,
    ) -> Result<Self> {
        Self::from_words_masked(istream, argv, files, dir, None)
    }

    /// The chain of `argv` with `mask` as the channels its selectors count
    /// (`build_effects_chain_from_file` with a channel mask).
    pub(crate) fn from_words_masked(
        istream: Stream,
        argv: &[&str],
        files: Option<&mut EffectsFiles>,
        dir: Option<String>,
        mask: Option<&[bool]>,
    ) -> Result<Self> {
        let mut chain = Chain::new(istream);
        let tokens: Vec<Token> = argv.iter().map(|w| lex(w)).collect();
        let mut state = ParseState::new(mask, istream.channels);
        state.dir = dir;
        let mut ctx = ParseCtx { files };
        let end = chain.parse(&mut state, &tokens, 0, false, 1, &mut ctx)?;
        debug_assert_eq!(end, tokens.len());
        chain.build()?;
        Ok(chain)
    }

    /// `ec_parse`: parses `tokens[i..]` up to the end, or up to the `}` that closes the block
    /// when `in_block`; returns the index where it stopped.
    fn parse(
        &mut self,
        st: &mut ParseState,
        tokens: &[Token],
        mut i: usize,
        in_block: bool,
        depth: usize,
        ctx: &mut ParseCtx,
    ) -> Result<usize> {
        if depth > MAX_PARSE_DEPTH {
            return Err(Error::Message("maximum recursion depth exceeded".into()));
        }
        while i < tokens.len() {
            let tok = tokens[i];
            if in_block && tok == Token::BlockEnd {
                return Ok(i);
            }
            match tok {
                Token::AllowFail => {
                    st.allow_fail = true;
                    i += 1;
                    continue;
                }
                Token::NewThread => {
                    self.new_sc = true;
                    i += 1;
                    continue;
                }
                _ => {}
            }
            let n = self.ostream.channels;
            if st.last_stream_ch != n {
                st.update_mask(n);
            }
            if let Token::ChSel(text) = tok {
                if st.last_stream_ch != n {
                    st.last_stream_ch = n;
                }
                st.ch_sel = parse_selector_masked(text, &st.ch_mask)?;
                st.last_ch_sel = Some(text.to_string());
                i += 1;
                continue;
            }
            if st.last_stream_ch != n {
                // the number of channels changed: parse the selector again
                st.ch_sel = match &st.last_ch_sel {
                    None => st.ch_mask.clone(),
                    Some(text) => parse_selector_masked(text, &st.ch_mask)?,
                };
                st.last_stream_ch = n;
            }
            match tok {
                Token::Source(path) => {
                    self.source(st, path, depth, ctx)?;
                    i += 1;
                    continue;
                }
                Token::BlockStart => {
                    let mut child = ParseState::new(Some(&st.ch_sel), n);
                    child.last_stream_ch = st.last_stream_ch;
                    child.dir = st.dir.clone();
                    let end = self.parse(&mut child, tokens, i + 1, true, depth + 1, ctx)?;
                    if end >= tokens.len() {
                        return Err(Error::Message("unterminated block".into()));
                    }
                    i = end + 1;
                    continue;
                }
                Token::Literal(_) => {}
                _ => return Err(Error::Message(format!("unexpected token: {}", tok.text()))),
            }
            let name = tok.text();
            let mut j = i + 1;
            while j < tokens.len() && !tokens[j].is_keyword() {
                j += 1;
            }
            let args: Vec<&str> = tokens[i + 1..j].iter().map(|t| t.text()).collect();
            let r = if is_parser_effect(name) {
                self.push_watch(st, &args, ctx)
            } else if get_effect_info(name).is_some() {
                self.push(name, &args, Some(&st.ch_sel))
            } else if UNAVAILABLE.contains(&name) {
                Err(Error::Unsupported(format!("effect not available: {name}")))
            } else {
                Err(Error::UnknownEffect(name.into()))
            };
            // `!`: an effect that fails to start is left out
            if let Err(e) = r {
                if !st.allow_fail {
                    return Err(e);
                }
            }
            st.allow_fail = false;
            i = j;
        }
        Ok(i)
    }

    /// `watch_effect_init`: a sub-chain from an effects file that the host can reload.
    fn push_watch(&mut self, st: &ParseState, args: &[&str], ctx: &mut ParseCtx) -> Result<()> {
        let Some(files) = ctx.files.as_deref_mut() else {
            return Err(Error::Unsupported(
                "watch without a file reader (Chain::from_args_with_files)".into(),
            ));
        };
        let mut argv = vec!["watch"];
        argv.extend_from_slice(args);
        let n_sel = st.ch_sel.iter().filter(|b| **b).count();
        let (fs, dir, home) = (self.ostream.fs, st.dir.clone(), files.home.clone());
        let resolve = |p: &str| full_path(dir.as_deref(), p, fs, n_sel, home.as_deref());
        let (e, handle) = crate::watch::make(self.ostream, &st.ch_sel, &argv, files, resolve)?;
        if self.new_sc {
            if !self.effects.is_empty() {
                self.barriers.push(self.effects.len());
            }
            self.new_sc = false;
        }
        self.ostream = e.ostream();
        self.effects.push(e);
        self.watches.push(handle);
        self.built = false;
        Ok(())
    }

    /// `ec_parse_file` for `@path`: reads the file and parses it as a block whose mask is the
    /// active selector; its relative paths start from its own directory.
    fn source(
        &mut self,
        st: &ParseState,
        path: &str,
        depth: usize,
        ctx: &mut ParseCtx,
    ) -> Result<()> {
        let n = self.ostream.channels;
        let Some(files) = ctx.files.as_deref_mut() else {
            return Err(Error::Unsupported(format!(
                "effects files (@{path}) without a file reader (Chain::from_args_with_files)"
            )));
        };
        let n_sel = st.ch_sel.iter().filter(|b| **b).count();
        let full = full_path(
            st.dir.as_deref(),
            path,
            self.ostream.fs,
            n_sel,
            files.home.as_deref(),
        );
        let text = (files.read)(&full).map_err(|e| {
            Error::Message(format!("error: failed to load effects file: {full}: {e}"))
        })?;
        let words = split_words(&text)?;
        let tokens: Vec<Token> = words.iter().map(|w| lex(w)).collect();
        let mut state = ParseState::new(Some(&st.ch_sel), n);
        state.dir = Some(
            full.rfind('/')
                .map_or_else(|| ".".to_string(), |b| full[..b].to_string()),
        );
        self.parse(&mut state, &tokens, 0, false, depth + 2, ctx)?;
        Ok(())
    }

    /// What the measuring effects (`stats`, `levels`) found, in chain order. A `stats`
    /// report covers the signal so far and ends its measurement, as the end of the signal
    /// does in the original; call it after [`Chain::drain`] (or [`Chain::process`]).
    pub fn reports(&mut self) -> Vec<Report> {
        self.effects.iter_mut().filter_map(|e| e.report()).collect()
    }

    /// Runs the interleaved signal `input` through the chain, `block_frames` at a time, and
    /// returns what comes out so far. Effects with latency hold back some frames; call
    /// [`Chain::drain`] after the last input to get them, or use [`Chain::process`].
    pub fn run(&mut self, input: &[f64]) -> Vec<f64> {
        if self.build().is_err() {
            return Vec::new();
        }
        let ich = self.istream.channels.max(1);
        let total = input.len() / ich;
        self.iframes += total;
        let mut out = Vec::with_capacity(total * self.ostream.channels);
        let mut start = 0;
        while start < total {
            let n = self.block_frames.min(total - start);
            self.run_block(&input[start * ich..(start + n) * ich], &mut out);
            start += n;
        }
        out
    }

    /// Makes the two buffers big enough for `n` input frames.
    fn ensure_buffers(&mut self, n: usize) {
        let ich = self.istream.channels.max(1);
        let max_ch = self
            .effects
            .iter()
            .map(|e| e.ostream().channels)
            .chain([ich])
            .max()
            .unwrap_or(ich);
        let mut cap = n;
        for e in &self.effects {
            cap = cap.max(e.buffer_frames(cap));
        }
        let len = cap * max_ch;
        if self.buf1.len() < len {
            self.buf1.resize(len, 0.0);
        }
        if self.buf2.len() < len {
            self.buf2.resize(len, 0.0);
        }
    }

    /// Runs one block through every effect and appends the result to `out`.
    fn run_block(&mut self, block: &[f64], out: &mut Vec<f64>) {
        let ich = self.istream.channels.max(1);
        let n = block.len() / ich;
        self.ensure_buffers(n);
        self.buf1[..n * ich].copy_from_slice(block);
        let (frames, in_first) = self.run_effects(0, n, true);
        self.oframes += frames;
        let result = if in_first { &self.buf1 } else { &self.buf2 };
        out.extend_from_slice(&result[..frames * self.ostream.channels]);
    }

    /// Runs the effects from `first` on over `frames` frames that lie in the first buffer
    /// (`in_first`) or the second. Returns the number of frames and where they are.
    fn run_effects(
        &mut self,
        first: usize,
        mut frames: usize,
        mut in_first: bool,
    ) -> (usize, bool) {
        for e in self.effects.iter_mut().skip(first) {
            let (a, b) = if in_first {
                (&mut self.buf1, &mut self.buf2)
            } else {
                (&mut self.buf2, &mut self.buf1)
            };
            if e.run(&mut frames, a, b) == Buf::Output {
                in_first = !in_first;
            }
        }
        (frames, in_first)
    }

    /// Flushes the chain after the last input (`drain_effects_chain`): feeds the silence that
    /// the effects need to pass on what they still hold, then asks the effects that hold more
    /// (the resampler) for it, and returns what comes out.
    pub fn drain(&mut self) -> Vec<f64> {
        let mut out = Vec::new();
        let block = self.block_frames;
        while let Some(part) = self.drain_once(block) {
            out.extend_from_slice(part);
        }
        out
    }

    /// One step of [`Chain::drain`] of at most `max_frames` frames, in the chain's own
    /// buffer; `None` when nothing is left (`drain_effects_chain` once).
    pub(crate) fn drain_once(&mut self, max_frames: usize) -> Option<&[f64]> {
        if self.effects.is_empty() || self.iframes < 1 || self.build().is_err() {
            return None;
        }
        let ich = self.istream.channels.max(1);
        let och = self.ostream.channels;
        if self.drain_frames > 0 {
            let n = max_frames.min(self.drain_frames);
            self.drain_frames -= n;
            self.ensure_buffers(n);
            self.buf1[..n * ich].fill(0.0);
            let (frames, in_first) = self.run_effects(0, n, true);
            self.oframes += frames;
            let result = if in_first { &self.buf1 } else { &self.buf2 };
            return Some(&result[..frames * och]);
        }
        // `drain_effect_list`: skip to the first effect that has something left, take what it
        // gives, and pass that through the effects after it.
        self.ensure_buffers(max_frames);
        let mut frames = max_frames as isize;
        let mut dframes = -1isize;
        let mut in_first = true;
        let mut idx = 0;
        while idx < self.effects.len() && dframes == -1 {
            let (a, b) = (&mut self.buf1, &mut self.buf2);
            let (a, b) = if in_first { (a, b) } else { (b, a) };
            dframes = frames;
            match self.effects[idx].drain2(&mut dframes, a, b) {
                Some(Buf::Output) => in_first = !in_first,
                Some(Buf::Input) => {}
                None => dframes = -1,
            }
            let (is_, os_) = (self.effects[idx].istream(), self.effects[idx].ostream());
            if is_.fs != os_.fs {
                let g = crate::effect::gcd(os_.fs, is_.fs);
                frames = crate::effect::ratio_mult_ceil(
                    frames as i64,
                    i64::from(os_.fs / g),
                    i64::from(is_.fs / g),
                ) as isize;
            }
            idx += 1;
        }
        if dframes <= 0 {
            return None;
        }
        let (n, in_first) = self.run_effects(idx, dframes as usize, in_first);
        self.oframes += n;
        let result = if in_first { &self.buf1 } else { &self.buf2 };
        Some(&result[..n * och])
    }

    /// Runs one block (all of `input`) through the chain and gives the result in the chain's
    /// own buffer (`run_effects_chain`). It allocates nothing once
    /// [`Chain::prepare_buffers`] has seen a block as long.
    pub(crate) fn run_once(&mut self, input: &[f64]) -> &[f64] {
        let ich = self.istream.channels.max(1);
        let n = input.len() / ich;
        if self.build().is_err() {
            return &[];
        }
        self.iframes += n;
        self.ensure_buffers(n);
        self.buf1[..n * ich].copy_from_slice(&input[..n * ich]);
        let (frames, in_first) = self.run_effects(0, n, true);
        self.oframes += frames;
        let result = if in_first { &self.buf1 } else { &self.buf2 };
        &result[..frames * self.ostream.channels]
    }

    /// Sizes the buffers for blocks of up to `frames` input frames.
    pub(crate) fn prepare_buffers(&mut self, frames: usize) {
        self.ensure_buffers(frames);
    }

    /// The longest output for `frames` input frames (`get_effects_chain_max_out_frames`).
    pub(crate) fn max_out_frames(&self, frames: usize) -> usize {
        self.effects.iter().fold(frames, |n, e| e.buffer_frames(n))
    }

    /// How many frames have come out so far (`oframes`).
    pub(crate) fn oframes(&self) -> usize {
        self.oframes
    }

    /// The input stream.
    pub fn istream(&self) -> Stream {
        self.istream
    }

    /// Runs `input` through the chain and flushes it: what `dsp` writes for a whole file.
    pub fn process(&mut self, input: &[f64]) -> Vec<f64> {
        let mut out = self.run(input);
        out.extend(self.drain());
        out
    }

    /// Whether any effect changes the signal in a way that dither at the output would help
    /// (`effects_chain_needs_dither`).
    pub fn needs_dither(&self) -> bool {
        self.effects
            .iter()
            .any(|e| !e.no_dither() && !e.is_dither())
    }

    /// Tells the dither effects the precision of the output and whether dithering is wanted
    /// (`effects_chain_set_dither_params`). Returns whether the caller should add dither of its
    /// own at the output, which is the case when there is no dither effect after the last
    /// effect that needs it.
    pub fn set_dither_params(&mut self, bits: i32, enabled: bool) -> bool {
        let mut r = true;
        for e in self.effects.iter_mut() {
            if e.set_dither_params(bits, enabled) {
                r = false;
            } else if !e.no_dither() {
                r = true;
            }
        }
        r && enabled
    }

    /// Forgets the signal in every effect.
    pub fn reset(&mut self) {
        for e in self.effects.iter_mut() {
            e.reset();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Stream = Stream {
        fs: 48_000,
        channels: 2,
    };

    fn ramp() -> Vec<f64> {
        (0..200).map(|i| f64::from(i) / 400.0).collect()
    }

    #[test]
    fn a_block_counts_its_selectors_within_its_mask() {
        let x = ramp();
        let mut a = Chain::from_args(S, &[":1", "{", ":0", "gain", "-6", "}"]).unwrap();
        let mut b = Chain::from_args(S, &[":1", "gain", "-6"]).unwrap();
        assert_eq!(a.process(&x), b.process(&x));
        // a selector inside the block ends with it
        let mut a = Chain::from_args(S, &["{", ":0", "gain", "-6", "}", "gain", "-6"]).unwrap();
        let y = a.process(&x);
        assert!((y[0] - x[0] * 0.25).abs() < 1e-3 && (y[1] - x[1] * 0.5).abs() < 1e-3);
    }

    #[test]
    fn badly_formed_blocks_and_unsupported_directives_are_errors() {
        assert!(Chain::from_args(S, &["{", "gain", "-3"]).is_err());
        assert!(Chain::from_args(S, &["gain", "-3", "}"]).is_err());
        assert!(matches!(
            Chain::from_args(S, &["zita_convolver", "x"]),
            Err(Error::Unsupported(_))
        ));
        // as in the original, a selector that no longer fits after the block shrank the stream
        assert!(Chain::from_args(
            S,
            &[
                "remix", "0", "1", "0", ":1,2", "{", "remix", "0,1", ":0", "gain", "2", "}",
                "gain", "-1"
            ]
        )
        .is_err());
    }

    #[test]
    fn an_effect_marked_with_a_bang_may_fail() {
        assert!(Chain::from_args(S, &["nonexistent", "gain", "-3"]).is_err());
        assert!(Chain::from_args(S, &["!", "nonexistent", "1", "gain", "-3"]).is_ok());
        assert!(Chain::from_args(S, &["!", "lowpass", "99999k", "0.7q", "gain", "-3"]).is_ok());
        // the `!` is for the next effect only
        assert!(Chain::from_args(S, &["!", "gain", "-3", "lowpass", "99999k", "0.7q"]).is_err());
    }

    #[test]
    fn a_backslash_makes_an_effect_name_an_argument() {
        // without the backslash `gain` starts a new effect and `remix` has no arguments
        assert!(Chain::from_args(S, &["remix", "gain"]).is_err());
        assert!(matches!(
            Chain::from_args(S, &["remix", "\\gain"]),
            Err(Error::Parse { .. }) | Err(Error::Message(_)) | Err(Error::Range { .. })
        ));
        assert_eq!(lex("\\:0"), Token::Escaped(":0"));
        assert!(!lex("\\gain").is_keyword() && lex("gain").is_keyword());
    }

    #[test]
    fn effects_file_words_follow_the_original_lexer() {
        let w = |t: &str| split_words(t).unwrap();
        assert_eq!(
            w("gain -3  # comment\nlowpass 1k"),
            ["gain", "-3", "lowpass", "1k"]
        );
        assert_eq!(w("a#b \"c d\" \"\""), ["a#b", "c d", ""]);
        // a backslash in a word makes the next character literal; at the start it stays
        assert_eq!(w(r"a\ b \#x \\y"), ["a b", r"\#x", r"\\y"]);
        assert_eq!(w("# only a comment"), Vec::<String>::new());
        assert!(split_words("gain \"-3").is_err());
    }

    #[test]
    fn paths_get_the_original_substitutions() {
        assert_eq!(
            full_path(Some("d"), "x_%r_%k_%c.txt", 44100, 2, None),
            "d/x_44100_44.1_2.txt"
        );
        assert_eq!(
            full_path(Some("d"), "/abs/%%%z%", 48000, 1, None),
            "/abs/%%z%"
        );
        assert_eq!(
            full_path(Some("d"), "~/fx", 48000, 1, Some("/home/u")),
            "/home/u/fx"
        );
        assert_eq!(full_path(None, "rel", 48000, 1, None), "rel");
    }

    #[test]
    fn effects_files_are_read_through_the_host() {
        let x = ramp();
        assert!(matches!(
            Chain::from_args(S, &["@fx.txt"]),
            Err(Error::Unsupported(_))
        ));
        let mut read = |p: &str| match p {
            "base/fx.txt" => Ok(":0 gain -6 @sub/more.txt".to_string()),
            "base/sub/more.txt" => Ok("# relative to its own directory\ngain -6".to_string()),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                p.to_string(),
            )),
        };
        let mut files = EffectsFiles {
            dir: Some("base".into()),
            home: None,
            read: &mut read,
        };
        let mut a = Chain::from_args_with_files(S, &[":1", "@fx.txt"], &mut files).unwrap();
        let mut b = Chain::from_args(S, &[":1", "gain", "-12"]).unwrap();
        let (ya, yb) = (a.process(&x), b.process(&x));
        assert!(ya.iter().zip(&yb).all(|(p, q)| (p - q).abs() < 1e-12));
        assert!(Chain::from_args_with_files(S, &["@missing.txt"], &mut files).is_err());
    }

    #[test]
    fn new_thread_only_keeps_the_optimiser_from_merging_across_it() {
        let x = ramp();
        let len = |a: &[&str]| Chain::from_args(S, a).unwrap().process(&x).len();
        // merged, the two fractional delays are one all-pass of the higher order; apart, two
        assert!(
            len(&["delay", "-f", "2.3S", "delay", "-f", "1.4S"])
                < len(&["delay", "-f", "2.3S", "new_thread", "delay", "-f", "1.4S"])
        );
        let mut a =
            Chain::from_args(S, &["lowpass", "1k", "0.7q", "new_thread", "gain", "-3"]).unwrap();
        let mut b = Chain::from_args(S, &["lowpass", "1k", "0.7q", "gain", "-3"]).unwrap();
        assert_eq!(a.process(&x), b.process(&x));
    }
}
