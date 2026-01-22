/*! UI module for soundlog-gui

Two-pane layout:
- left: VGM AST tree (supports lazy-loading of many command children)
- right: binary hex viewer (painter-based)

Strategy:
- On initial parse (background) we build a lightweight AST that contains a
  `Commands` node annotated with the total number of commands but *without*
  allocating child widgets/strings for each command.
- When the user expands `Commands` (or presses "Show more"), the UI requests
  a chunk of command nodes to be generated in a background thread. The
  background worker reparses (from bytes) and formats only the requested
  range into `AstNode`s, then sends them back to the UI which appends them
  into an in-memory chunk for incremental display.

This avoids doing large string allocation and widget construction on the UI
thread all at once and keeps the UI responsive for very large VGM files.
*/

use crate::gui::HexViewer;
use eframe::egui;

use soundlog::VgmDocument;
use soundlog::vgm::VgmHeaderField;

use std::collections::HashMap;
use std::sync::mpsc;
use std::thread;

/// Simple AST node representation for the UI.
/// `lazy_count` is Some(n) when this node is a placeholder for many children
/// (e.g. the `Commands` node) and children are fetched lazily.
#[derive(Clone, Debug)]
pub struct AstNode {
    pub title: String,
    pub detail: String,
    pub children: Vec<AstNode>,
    pub lazy_count: Option<usize>,
    /// If this lazy node corresponds to a specific range (a bucket), this holds
    /// the absolute start index in the command list for the bucket. Used when
    /// requesting children for that bucket.
    pub lazy_start: Option<usize>,
    /// Optional byte range (start, len) this AST node corresponds to in the raw
    /// file bytes. When present, clicking the node will highlight this range
    /// in the hex viewer.
    pub byte_range: Option<(usize, usize)>,
}

impl AstNode {
    pub fn new(title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            detail: detail.into(),
            children: Vec::new(),
            lazy_count: None,
            lazy_start: None,
            byte_range: None,
        }
    }

    pub fn with_children(mut self, children: Vec<AstNode>) -> Self {
        self.children = children;
        self
    }

    #[allow(dead_code)]
    pub fn with_lazy(mut self, count: usize) -> Self {
        self.lazy_count = Some(count);
        self
    }

    /// Mark this node as a lazy range starting at `start` and with `count` items.
    pub fn with_lazy_range(mut self, start: usize, count: usize) -> Self {
        self.lazy_count = Some(count);
        self.lazy_start = Some(start);
        self
    }

    /// Attach a byte range (start offset, length) to this node.
    pub fn with_byte_range(mut self, start: usize, len: usize) -> Self {
        self.byte_range = Some((start, len));
        self
    }
}

/// Messages sent from background workers to the UI.
///
/// - `Full` contains the entire prebuilt lightweight AST (header + Commands
///   node with lazy_count set, not necessarily filled children).
/// - `Partial` contains a chunk of children for a node identified by `path`.
/// - `Error` contains a user-presentable error message.
pub enum AstBuildMessage {
    Full(Vec<AstNode>),
    Partial {
        path: Vec<usize>,
        start: usize,
        nodes: Vec<AstNode>,
    },
    /// Differences between original file bytes and the serialized/rebuilt bytes.
    /// Each tuple is (start_inclusive, end_inclusive).
    /// The `Diff` variant now carries the rebuilt bytes as well so the UI can
    /// display both original and rebuilt data when needed.
    Diff(Vec<(usize, usize)>, Vec<u8>),
    Error(String),
}

/// UI state holding AST, raw bytes and supporting maps for lazy-loading.
pub struct UiState {
    pub ast_root: Vec<AstNode>,
    pub bytes: Vec<u8>,
    pub selected_ast: Option<Vec<usize>>,
    /// The last observed selected AST label rect (widget coords). Used to
    /// scroll the left pane so keyboard-driven selection is visible.
    pub last_selected_ast_rect: Option<egui::Rect>,
    /// When keyboard navigation changes selection, this stores the path that
    /// should receive focus/scroll. Cleared once applied in the drawing pass.
    pub pending_focus: Option<Vec<usize>>,
    /// Last widget Id that requested focus by keyboard/interaction. Used to
    /// re-apply focus (for example to suppress TAB-based focus changes).
    pub last_focused_widget: Option<egui::Id>,
    pub hex_viewer: HexViewer,
    /// If a background parse produced rebuilt/serialized bytes (used to detect diffs),
    /// keep them here so UI components can access both original (`bytes`) and rebuilt bytes.
    pub rebuilt_bytes: Option<Vec<u8>>,

    /// Channel receiver to accept background build messages (full or partial).
    pub ast_build_rx: Option<mpsc::Receiver<AstBuildMessage>>,
    /// Channel sender to be cloned and used by background tasks.
    pub ast_build_tx: Option<mpsc::Sender<AstBuildMessage>>,

    /// Whether an initial parse is in progress.
    pub ast_building: bool,

    /// For lazy nodes (keyed by path string like "0" or "1.2"), store the already
    /// loaded child nodes in display order (appended as partial chunks arrive).
    pub loaded_lazy_nodes: HashMap<String, Vec<AstNode>>,

    /// Prevent duplicate concurrent requests per node path.
    pub pending_requests: HashMap<String, bool>,

    /// Chunk size for lazy loading (number of commands to request per click).
    pub lazy_chunk_size: usize,

    /// Deferred loads collected during UI drawing. These are executed after
    /// drawing to avoid multiple mutable borrows during recursive rendering.
    /// Each tuple is (path, start_relative, count).
    pub deferred_loads: Vec<(Vec<usize>, usize, usize)>,

    /// Temporary set of enqueued requests to prevent duplicate deferred loads.
    pub enqueued_requests: HashMap<String, bool>,
}

impl UiState {
    #[allow(dead_code)]
    pub fn new_with_placeholders() -> Self {
        let ast_root = vec![
            AstNode::new("VGM Header", "Header fields and metadata").with_children(vec![
                AstNode::new("Ident", "VgmIdent: 'Vgm '"),
                AstNode::new("Version", "0x00000150"),
            ]),
            AstNode::new("Commands", "No commands loaded").with_lazy(0),
        ];

        let bytes = (0u8..=255u8).collect::<Vec<u8>>();

        Self {
            ast_root,
            bytes,
            selected_ast: None,
            last_selected_ast_rect: None,
            pending_focus: None,
            last_focused_widget: None,
            hex_viewer: HexViewer::new(),
            rebuilt_bytes: None,
            ast_build_rx: None,
            ast_build_tx: None,
            ast_building: false,
            loaded_lazy_nodes: HashMap::new(),
            pending_requests: HashMap::new(),
            lazy_chunk_size: 200,
            deferred_loads: Vec::new(),
            enqueued_requests: HashMap::new(),
        }
    }

    pub fn new_empty() -> Self {
        Self {
            ast_root: Vec::new(),
            bytes: Vec::new(),
            selected_ast: None,
            last_selected_ast_rect: None,
            pending_focus: None,
            last_focused_widget: None,
            hex_viewer: HexViewer::new(),
            rebuilt_bytes: None,
            ast_build_rx: None,
            ast_build_tx: None,
            ast_building: false,
            loaded_lazy_nodes: HashMap::new(),
            pending_requests: HashMap::new(),
            lazy_chunk_size: 200,
            deferred_loads: Vec::new(),
            enqueued_requests: HashMap::new(),
        }
    }

    /// Push an event string into the recent_events buffer (kept as a no-op in
    /// non-debug builds).
    #[allow(dead_code)]
    fn push_event(&mut self, _ev: impl Into<String>) {
        // Intentionally left empty: UI-level event logging removed for release build.
    }

    /// Build the `Header` top-level AST node (with child fields and byte ranges).
    ///
    /// This extracts the header construction logic from the background worker so
    /// the closure remains small. It returns a fully-populated `AstNode` for
    /// the Header (including byte_range when determinable).
    fn build_header_node(doc: &VgmDocument) -> AstNode {
        // Build header child nodes (only non-zero/meaningful fields except Ident which is always shown).
        let mut header_children: Vec<AstNode> = Vec::new();

        // Always show ident even if it's zero-filled
        header_children.push(
            AstNode::new(
                "Ident",
                format!("'{}'", String::from_utf8_lossy(&doc.header.ident)),
            )
            // VGM ident occupies bytes 0x00..0x03 (4 bytes)
            .with_byte_range(0x00, 4),
        );

        if doc.header.eof_offset != 0 {
            header_children.push(AstNode::new(
                "EOF offset",
                format!("0x{:08x}", doc.header.eof_offset),
            ));
        }
        if doc.header.version != 0 {
            header_children.push(AstNode::new(
                "Version",
                format!("0x{:08x}", doc.header.version),
            ));
        }
        if doc.header.sn76489_clock != 0 {
            header_children.push(AstNode::new(
                "SN76489 clock",
                format!("{}", doc.header.sn76489_clock),
            ));
        }
        if doc.header.ym2413_clock != 0 {
            header_children.push(AstNode::new(
                "YM2413 clock",
                format!("{}", doc.header.ym2413_clock),
            ));
        }
        if doc.header.gd3_offset != 0 {
            header_children.push(AstNode::new(
                "GD3 offset",
                format!("0x{:08x}", doc.header.gd3_offset),
            ));
        }
        if doc.header.total_samples != 0 {
            header_children.push(AstNode::new(
                "Total samples",
                format!("{}", doc.header.total_samples),
            ));
        }
        if doc.header.loop_offset != 0 {
            header_children.push(AstNode::new(
                "Loop offset",
                format!("0x{:08x}", doc.header.loop_offset),
            ));
        }
        if doc.header.loop_samples != 0 {
            header_children.push(AstNode::new(
                "Loop samples",
                format!("{}", doc.header.loop_samples),
            ));
        }
        if doc.header.sample_rate != 0 {
            header_children.push(AstNode::new(
                "Sample rate",
                format!("{}", doc.header.sample_rate),
            ));
        }
        if doc.header.sn_fb != 0 {
            header_children.push(AstNode::new("SN FB", format!("{}", doc.header.sn_fb)));
        }
        if doc.header.snw != 0 {
            header_children.push(AstNode::new("SNW", format!("{}", doc.header.snw)));
        }
        if doc.header.sf != 0 {
            header_children.push(AstNode::new("SF", format!("{}", doc.header.sf)));
        }
        if doc.header.ym2612_clock != 0 {
            header_children.push(AstNode::new(
                "YM2612 clock",
                format!("{}", doc.header.ym2612_clock),
            ));
        }
        if doc.header.ym2151_clock != 0 {
            header_children.push(AstNode::new(
                "YM2151 clock",
                format!("{}", doc.header.ym2151_clock),
            ));
        }
        if doc.header.data_offset != 0 {
            header_children.push(AstNode::new(
                "Data offset",
                format!("0x{:08x}", doc.header.data_offset),
            ));
        }
        if doc.header.sega_pcm_clock != 0 {
            header_children.push(AstNode::new(
                "Sega PCM clock",
                format!("{}", doc.header.sega_pcm_clock),
            ));
        }
        if doc.header.spcm_interface != 0 {
            header_children.push(AstNode::new(
                "SPCM interface",
                format!("{}", doc.header.spcm_interface),
            ));
        }
        if doc.header.rf5c68_clock != 0 {
            header_children.push(AstNode::new(
                "RF5C68 clock",
                format!("{}", doc.header.rf5c68_clock),
            ));
        }
        if doc.header.ym2203_clock != 0 {
            header_children.push(AstNode::new(
                "YM2203 clock",
                format!("{}", doc.header.ym2203_clock),
            ));
        }
        if doc.header.ym2608_clock != 0 {
            header_children.push(AstNode::new(
                "YM2608 clock",
                format!("{}", doc.header.ym2608_clock),
            ));
        }
        if doc.header.ym2610b_clock != 0 {
            header_children.push(AstNode::new(
                "YM2610B clock",
                format!("{}", doc.header.ym2610b_clock),
            ));
        }
        if doc.header.ym3812_clock != 0 {
            header_children.push(AstNode::new(
                "YM3812 clock",
                format!("{}", doc.header.ym3812_clock),
            ));
        }
        if doc.header.ym3526_clock != 0 {
            header_children.push(AstNode::new(
                "YM3526 clock",
                format!("{}", doc.header.ym3526_clock),
            ));
        }
        if doc.header.y8950_clock != 0 {
            header_children.push(AstNode::new(
                "Y8950 clock",
                format!("{}", doc.header.y8950_clock),
            ));
        }
        if doc.header.ymf262_clock != 0 {
            header_children.push(AstNode::new(
                "YMF262 clock",
                format!("{}", doc.header.ymf262_clock),
            ));
        }
        if doc.header.ymf278b_clock != 0 {
            header_children.push(AstNode::new(
                "YMF278B clock",
                format!("{}", doc.header.ymf278b_clock),
            ));
        }
        if doc.header.ymf271_clock != 0 {
            header_children.push(AstNode::new(
                "YMF271 clock",
                format!("{}", doc.header.ymf271_clock),
            ));
        }
        if doc.header.ymz280b_clock != 0 {
            header_children.push(AstNode::new(
                "YMZ280B clock",
                format!("{}", doc.header.ymz280b_clock),
            ));
        }
        if doc.header.rf5c164_clock != 0 {
            header_children.push(AstNode::new(
                "RF5C164 clock",
                format!("{}", doc.header.rf5c164_clock),
            ));
        }
        if doc.header.pwm_clock != 0 {
            header_children.push(AstNode::new(
                "PWM clock",
                format!("{}", doc.header.pwm_clock),
            ));
        }
        if doc.header.ay8910_clock != 0 {
            header_children.push(AstNode::new(
                "AY8910 clock",
                format!("{}", doc.header.ay8910_clock),
            ));
        }
        if doc.header.ay_misc != [0u8; 8] {
            header_children.push(AstNode::new(
                "AY miscellaneous",
                format!("{:?}", doc.header.ay_misc),
            ));
        }
        if doc.header.gb_dmg_clock != 0 {
            header_children.push(AstNode::new(
                "GB DMG clock",
                format!("{}", doc.header.gb_dmg_clock),
            ));
        }
        if doc.header.nes_apu_clock != 0 {
            header_children.push(AstNode::new(
                "NES APU clock",
                format!("{}", doc.header.nes_apu_clock),
            ));
        }
        if doc.header.multipcm_clock != 0 {
            header_children.push(AstNode::new(
                "MultiPCM clock",
                format!("{}", doc.header.multipcm_clock),
            ));
        }
        if doc.header.upd7759_clock != 0 {
            header_children.push(AstNode::new(
                "UPD7759 clock",
                format!("{}", doc.header.upd7759_clock),
            ));
        }
        if doc.header.okim6258_clock != 0 {
            header_children.push(AstNode::new(
                "OKIM6258 clock",
                format!("{}", doc.header.okim6258_clock),
            ));
        }
        if doc.header.okim6258_flags != [0u8; 4] {
            header_children.push(AstNode::new(
                "OKIM6258 flags",
                format!("{:?}", doc.header.okim6258_flags),
            ));
        }
        if doc.header.okim6295_clock != 0 {
            header_children.push(AstNode::new(
                "OKIM6295 clock",
                format!("{}", doc.header.okim6295_clock),
            ));
        }
        if doc.header.k051649_clock != 0 {
            header_children.push(AstNode::new(
                "K051649 clock",
                format!("{}", doc.header.k051649_clock),
            ));
        }
        if doc.header.k054539_clock != 0 {
            header_children.push(AstNode::new(
                "K054539 clock",
                format!("{}", doc.header.k054539_clock),
            ));
        }
        if doc.header.huc6280_clock != 0 {
            header_children.push(AstNode::new(
                "HuC6280 clock",
                format!("{}", doc.header.huc6280_clock),
            ));
        }
        if doc.header.c140_clock != 0 {
            header_children.push(AstNode::new(
                "C140 clock",
                format!("{}", doc.header.c140_clock),
            ));
        }
        if doc.header.k053260_clock != 0 {
            header_children.push(AstNode::new(
                "K053260 clock",
                format!("{}", doc.header.k053260_clock),
            ));
        }
        if doc.header.pokey_clock != 0 {
            header_children.push(AstNode::new(
                "Pokey clock",
                format!("{}", doc.header.pokey_clock),
            ));
        }
        if doc.header.qsound_clock != 0 {
            header_children.push(AstNode::new(
                "QSound clock",
                format!("{}", doc.header.qsound_clock),
            ));
        }
        if doc.header.scsp_clock != 0 {
            header_children.push(AstNode::new(
                "SCSP clock",
                format!("{}", doc.header.scsp_clock),
            ));
        }
        if doc.header.extra_header_offset != 0 {
            header_children.push(AstNode::new(
                "Extra header offset",
                format!("0x{:08x}", doc.header.extra_header_offset),
            ));
        }
        if doc.header.wonderswan_clock != 0 {
            header_children.push(AstNode::new(
                "WonderSwan clock",
                format!("{}", doc.header.wonderswan_clock),
            ));
        }
        if doc.header.vsu_clock != 0 {
            header_children.push(AstNode::new(
                "VSU clock",
                format!("{}", doc.header.vsu_clock),
            ));
        }
        if doc.header.saa1099_clock != 0 {
            header_children.push(AstNode::new(
                "SAA1099 clock",
                format!("{}", doc.header.saa1099_clock),
            ));
        }
        if doc.header.es5503_clock != 0 {
            header_children.push(AstNode::new(
                "ES5503 clock",
                format!("{}", doc.header.es5503_clock),
            ));
        }
        if doc.header.es5506_clock != 0 {
            header_children.push(AstNode::new(
                "ES5506 clock",
                format!("{}", doc.header.es5506_clock),
            ));
        }
        if doc.header.es5506_channels != 0 {
            header_children.push(AstNode::new(
                "ES5506 channels",
                format!("{}", doc.header.es5506_channels),
            ));
        }
        if doc.header.es5506_cd != 0 {
            header_children.push(AstNode::new(
                "ES5506 CD flags",
                format!("{}", doc.header.es5506_cd),
            ));
        }
        if doc.header.es5506_reserved != 0 {
            header_children.push(AstNode::new(
                "ES5506 reserved",
                format!("{}", doc.header.es5506_reserved),
            ));
        }
        if doc.header.x1_010_clock != 0 {
            header_children.push(AstNode::new(
                "X1-010 clock",
                format!("{}", doc.header.x1_010_clock),
            ));
        }
        if doc.header.c352_clock != 0 {
            header_children.push(AstNode::new(
                "C352 clock",
                format!("{}", doc.header.c352_clock),
            ));
        }
        if doc.header.ga20_clock != 0 {
            header_children.push(AstNode::new(
                "GA20 clock",
                format!("{}", doc.header.ga20_clock),
            ));
        }
        if doc.header.mikey_clock != 0 {
            header_children.push(AstNode::new(
                "Mikey clock",
                format!("{}", doc.header.mikey_clock),
            ));
        }
        if doc.header.reserved_e8_ef != [0u8; 8] {
            header_children.push(AstNode::new(
                "Reserved E8-EF",
                format!("{:?}", doc.header.reserved_e8_ef),
            ));
        }
        if doc.header.reserved_f0_ff != [0u8; 16] {
            header_children.push(AstNode::new(
                "Reserved F0-FF",
                format!("{:?}", doc.header.reserved_f0_ff),
            ));
        }

        // Attach byte ranges to header child nodes using HeaderField mapping.
        // We map the node title to a HeaderField and, if the field exists in
        // the serialized header (per data_offset/version rules), attach the
        // computed byte range to the AstNode so the hex viewer can highlight it.
        let mappings: Vec<(&str, VgmHeaderField)> = vec![
            ("Ident", VgmHeaderField::Ident),
            ("EOF offset", VgmHeaderField::EofOffset),
            ("Version", VgmHeaderField::Version),
            ("SN76489 clock", VgmHeaderField::Sn76489Clock),
            ("YM2413 clock", VgmHeaderField::Ym2413Clock),
            ("GD3 offset", VgmHeaderField::Gd3Offset),
            ("Total samples", VgmHeaderField::TotalSamples),
            ("Loop offset", VgmHeaderField::LoopOffset),
            ("Loop samples", VgmHeaderField::LoopSamples),
            ("Sample rate", VgmHeaderField::SampleRate),
            ("SN FB", VgmHeaderField::SnFb),
            ("SNW", VgmHeaderField::Snw),
            ("SF", VgmHeaderField::Sf),
            ("YM2612 clock", VgmHeaderField::Ym2612Clock),
            ("YM2151 clock", VgmHeaderField::Ym2151Clock),
            ("Data offset", VgmHeaderField::DataOffset),
            ("Sega PCM clock", VgmHeaderField::SegaPcmClock),
            ("SPCM interface", VgmHeaderField::SpcmInterface),
            ("RF5C68 clock", VgmHeaderField::Rf5c68Clock),
            ("YM2203 clock", VgmHeaderField::Ym2203Clock),
            ("YM2608 clock", VgmHeaderField::Ym2608Clock),
            ("YM2610B clock", VgmHeaderField::Ym2610bClock),
            ("YM3812 clock", VgmHeaderField::Ym3812Clock),
            ("YM3526 clock", VgmHeaderField::Ym3526Clock),
            ("Y8950 clock", VgmHeaderField::Y8950Clock),
            ("YMF262 clock", VgmHeaderField::Ymf262Clock),
            ("YMF278B clock", VgmHeaderField::Ymf278bClock),
            ("YMF271 clock", VgmHeaderField::Ymf271Clock),
            ("YMZ280B clock", VgmHeaderField::Ymz280bClock),
            ("RF5C164 clock", VgmHeaderField::Rf5c164Clock),
            ("PWM clock", VgmHeaderField::PwmClock),
            ("AY8910 clock", VgmHeaderField::Ay8910Clock),
            ("AY miscellaneous", VgmHeaderField::AyMisc),
            ("GB DMG clock", VgmHeaderField::GbDmgClock),
            ("NES APU clock", VgmHeaderField::NesApuClock),
            ("MultiPCM clock", VgmHeaderField::MultipcmClock),
            ("UPD7759 clock", VgmHeaderField::Upd7759Clock),
            ("OKIM6258 clock", VgmHeaderField::Okim6258Clock),
            ("OKIM6258 flags", VgmHeaderField::Okim6258Flags),
            ("OKIM6295 clock", VgmHeaderField::Okim6295Clock),
            ("K051649 clock", VgmHeaderField::K051649Clock),
            ("K054539 clock", VgmHeaderField::K054539Clock),
            ("HuC6280 clock", VgmHeaderField::Huc6280Clock),
            ("C140 clock", VgmHeaderField::C140Clock),
            ("K053260 clock", VgmHeaderField::K053260Clock),
            ("Pokey clock", VgmHeaderField::PokeyClock),
            ("QSound clock", VgmHeaderField::QsoundClock),
            ("SCSP clock", VgmHeaderField::ScspClock),
            ("Extra header offset", VgmHeaderField::ExtraHeaderOffset),
            ("WonderSwan clock", VgmHeaderField::WonderSwan),
            ("VSU clock", VgmHeaderField::Vsu),
            ("SAA1099 clock", VgmHeaderField::Saa1099),
            ("ES5503 clock", VgmHeaderField::Es5503),
            ("ES5506 clock", VgmHeaderField::Es5506),
            ("ES5506 channels", VgmHeaderField::Es5506Channels),
            ("ES5506 CD flags", VgmHeaderField::Es5506Cd),
            ("ES5506 reserved", VgmHeaderField::Es5506Reserved),
            ("X1-010 clock", VgmHeaderField::X1_010),
            ("C352 clock", VgmHeaderField::C352),
            ("GA20 clock", VgmHeaderField::Ga20),
            ("Mikey clock", VgmHeaderField::Mikey),
            ("Reserved E8-EF", VgmHeaderField::ReservedE8EF),
            ("Reserved F0-FF", VgmHeaderField::ReservedF0FF),
        ];

        for node in &mut header_children {
            if let Some((_, hf)) = mappings.iter().find(|(title, _)| *title == node.title)
                && let Some((start, len)) =
                    hf.byte_range(doc.header.version, doc.header.data_offset)
            {
                node.byte_range = Some((start, len));
            }
        }

        // Attach header node and compute its overall byte range when possible.
        // Prefer the first command absolute offset as the header length if commands exist;
        // otherwise fall back to GD3 start if present.
        let mut header_node =
            AstNode::new("Header", "Header fields").with_children(header_children);
        let header_len_opt = if !doc.commands.is_empty() {
            // souecemap() returns absolute (file) offsets for commands; the first command's
            // absolute offset equals the serialized header length. Use that when available.
            doc.sourcemap().first().map(|(off, _)| *off)
        } else if doc.header.gd3_offset != 0 {
            // If there are no commands but GD3 exists, the GD3 start marks the end of header.
            Some(doc.header.gd3_offset.wrapping_add(0x14) as usize)
        } else {
            None
        };
        if let Some(hlen) = header_len_opt {
            header_node.byte_range = Some((0usize, hlen));
        }
        header_node
    }

    /// Build a GD3 top-level node (with child fields and byte ranges) if present.
    /// Returns Some(AstNode) when GD3 metadata exists and at least one child field
    /// is non-empty; otherwise returns None.
    fn build_gd3_node(doc: &VgmDocument) -> Option<AstNode> {
        if doc.gd3.is_some() && doc.header.gd3_offset != 0 {
            let gd3_start = doc.header.gd3_offset.wrapping_add(0x14) as usize;
            // Fields start after the 12-byte Gd3 header (ident+version+len).
            let mut field_off = gd3_start + 12_usize;
            let gd3_ref = doc.gd3.as_ref().unwrap();
            let mut gd3_children: Vec<AstNode> = Vec::new();

            // Helper to push a field node and advance the running offset.
            // For the Notes field we strip newlines so the AST/right-pane detail
            // shows a single-line note (per request).
            let push_field =
                |children: &mut Vec<AstNode>, title: &str, v: &Option<String>, off: &mut usize| {
                    if let Some(s) = v {
                        // UTF-16LE code units -> two bytes each
                        let len_bytes = s.encode_utf16().count() * 2;
                        // For Notes, remove newline characters; otherwise keep the original string.
                        let detail = if title == "Notes" {
                            s.replace('\n', " ")
                        } else {
                            s.clone()
                        };
                        children.push(AstNode::new(title, detail).with_byte_range(*off, len_bytes));
                        // advance past string bytes + 2-byte UTF-16 nul terminator
                        *off = off.saturating_add(len_bytes + 2);
                    } else {
                        // empty field: only the 2-byte terminator is present
                        *off = off.saturating_add(2);
                    }
                };

            push_field(
                &mut gd3_children,
                "Track name (EN)",
                &gd3_ref.track_name_en,
                &mut field_off,
            );
            push_field(
                &mut gd3_children,
                "Track name (JP)",
                &gd3_ref.track_name_jp,
                &mut field_off,
            );
            push_field(
                &mut gd3_children,
                "Game name (EN)",
                &gd3_ref.game_name_en,
                &mut field_off,
            );
            push_field(
                &mut gd3_children,
                "Game name (JP)",
                &gd3_ref.game_name_jp,
                &mut field_off,
            );
            push_field(
                &mut gd3_children,
                "System name (EN)",
                &gd3_ref.system_name_en,
                &mut field_off,
            );
            push_field(
                &mut gd3_children,
                "System name (JP)",
                &gd3_ref.system_name_jp,
                &mut field_off,
            );
            push_field(
                &mut gd3_children,
                "Author (EN)",
                &gd3_ref.author_name_en,
                &mut field_off,
            );
            push_field(
                &mut gd3_children,
                "Author (JP)",
                &gd3_ref.author_name_jp,
                &mut field_off,
            );
            push_field(
                &mut gd3_children,
                "Release date",
                &gd3_ref.release_date,
                &mut field_off,
            );
            push_field(
                &mut gd3_children,
                "Creator",
                &gd3_ref.creator,
                &mut field_off,
            );
            push_field(&mut gd3_children, "Notes", &gd3_ref.notes, &mut field_off);

            if !gd3_children.is_empty() {
                // Attach a GD3 top-level node and also record the full GD3 chunk range
                // so selecting the GD3 node highlights the entire metadata chunk.
                let mut gd3_node = AstNode::new("GD3", "Metadata").with_children(gd3_children);
                let gd3_len = doc.gd3.as_ref().map(|g| g.to_bytes().len()).unwrap_or(0);
                if gd3_len > 0 {
                    let gd3_start = doc.header.gd3_offset.wrapping_add(0x14) as usize;
                    gd3_node.byte_range = Some((gd3_start, gd3_len));
                }
                return Some(gd3_node);
            }
        }
        None
    }

    /// Kick off initial parse in background. This will produce a lightweight
    /// AST where the `Commands` node has `lazy_count = Some(total)`.
    pub fn populate_from_bytes(&mut self, bytes: &[u8]) {
        // store raw bytes
        self.bytes = bytes.to_vec();

        // If a background parse is already running, do nothing.
        if self.ast_building {
            return;
        }

        // Create a channel for background parse results if not already present.
        let (tx, rx) = mpsc::channel::<AstBuildMessage>();
        self.ast_build_rx = Some(rx);
        self.ast_build_tx = Some(tx.clone());
        self.ast_building = true;

        // Clone bytes to move into worker.
        let data = self.bytes.clone();

        // Spawn background thread to parse the document and produce the lightweight AST.
        thread::spawn(move || {
            match VgmDocument::try_from(data.as_slice()) {
                Ok(doc) => {
                    // Build header node (extracted helper).
                    let mut nodes: Vec<AstNode> = Vec::new();
                    let header_node = Self::build_header_node(&doc);
                    nodes.push(header_node);

                    // Commands node: create bucketed children (e.g. [0..1000], [1000..2000], ...)
                    // Each bucket is a lazy node that can be expanded to load its commands.
                    let total_cmds = doc.commands.len();
                    let bucket_size = 1000usize;
                    let mut buckets: Vec<AstNode> = Vec::new();
                    let mut start_idx = 0usize;
                    while start_idx < total_cmds {
                        let end_idx = std::cmp::min(start_idx + bucket_size, total_cmds);
                        let title = format!("[{}..{}]", start_idx, end_idx);
                        let detail = format!("{} commands", end_idx - start_idx);
                        // this bucket node is lazy and records its start index and count
                        buckets.push(
                            AstNode::new(title, detail)
                                .with_lazy_range(start_idx, end_idx - start_idx),
                        );
                        start_idx = end_idx;
                    }

                    // The top-level Commands node contains the bucket children (not lazy itself).
                    nodes.push(
                        AstNode::new("Commands", format!("{} commands", total_cmds))
                            .with_children(buckets),
                    );

                    // Place GD3 node after Commands so it appears below Commands in the AST.
                    if let Some(gd3_node) = Self::build_gd3_node(&doc) {
                        nodes.push(gd3_node);
                    }

                    let _ = tx.send(AstBuildMessage::Full(nodes));

                    // Compute differences between the original bytes (`data`) and the
                    // serialized/rebuilt bytes produced by the document serializer.
                    // `VgmDocument` implements `From<&VgmDocument> for Vec<u8>` so use
                    // `Vec::from(&doc)` rather than the private `to_bytes()` method.
                    let rebuilt_bytes = Vec::from(&doc);
                    let max_len = std::cmp::max(data.len(), rebuilt_bytes.len());
                    let mut diffs: Vec<(usize, usize)> = Vec::new();
                    let mut in_diff = false;
                    let mut diff_start: usize = 0;
                    for i in 0..max_len {
                        let orig = data.get(i);
                        let newb = rebuilt_bytes.get(i);
                        let differs = match (orig, newb) {
                            (Some(o), Some(n)) => o != n,
                            (Some(_), None) | (None, Some(_)) => true,
                            _ => false,
                        };
                        if differs {
                            if !in_diff {
                                in_diff = true;
                                diff_start = i;
                            }
                        } else if in_diff {
                            // close the current diff range (inclusive end)
                            diffs.push((diff_start, i.saturating_sub(1)));
                            in_diff = false;
                        }
                    }
                    if in_diff {
                        diffs.push((diff_start, max_len.saturating_sub(1)));
                    }

                    // Send diff ranges (may be empty) to the UI so it can render red overlays.
                    // Include rebuilt_bytes so the UI can present both original and rebuilt data
                    // in tooltips or other diagnostics views.
                    let _ = tx.send(AstBuildMessage::Diff(diffs, rebuilt_bytes));
                }
                Err(e) => {
                    let _ = tx.send(AstBuildMessage::Error(format!("{:?}", e)));
                }
            }
        });
    }

    /// Request a chunk of children for the node identified by `path`.
    /// - `start` is the first command index to build (relative to the bucket if
    ///   the node is a bucket; otherwise absolute).
    /// - `count` is how many commands to format.
    ///
    /// This spawns a background worker which reparses the VGM bytes and produces
    /// formatted `AstNode`s for the specified range. Results are sent via the
    /// shared sender stored in `ast_build_tx`. Note: the `start` in the
    /// `AstBuildMessage::Partial` is the *relative* offset within the bucket so
    /// the UI can insert the returned chunk at the correct position; the
    /// background worker will use absolute indices for parsing.
    pub fn request_children(&mut self, path: Vec<usize>, start: usize, count: usize) {
        // Build a stable key from path for bookkeeping.
        let path_key = path
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(".");

        // Avoid duplicate concurrent requests for the same path.
        if self
            .pending_requests
            .get(&path_key)
            .copied()
            .unwrap_or(false)
        {
            self.push_event(format!("request skipped (pending): {}", path_key));
            return;
        }

        // Ensure we have bytes to parse and a sender to send results.
        if self.bytes.is_empty() {
            self.push_event("request skipped: no bytes".to_string());
            return;
        }
        let tx_opt = self.ast_build_tx.clone();
        if tx_opt.is_none() {
            self.push_event("request skipped: no tx".to_string());
            return;
        }
        let tx = tx_opt.unwrap();

        // Mark a pending request.
        self.pending_requests.insert(path_key.clone(), true);
        self.push_event(format!(
            "request: {} start={} count={}",
            path_key, start, count
        ));

        // Clone bytes to move into thread.
        let data = self.bytes.clone();

        // Determine base absolute start for this path (if the node corresponds to a bucket).
        // If the node at `path` has a `lazy_start`, treat the provided `start` as
        // relative to that bucket; otherwise `start` is absolute.
        let mut base_abs = 0usize;
        let mut cur_nodes = &self.ast_root;
        for idx in &path {
            if *idx >= cur_nodes.len() {
                break;
            }
            let node = &cur_nodes[*idx];
            if let Some(ls) = node.lazy_start {
                base_abs = ls;
            }
            cur_nodes = &node.children;
        }
        // Keep the relative start for returning in the Partial message.
        let relative_start = start;
        // Compute absolute start for parsing.
        let absolute_start = base_abs.saturating_add(relative_start);

        thread::spawn(move || {
            // Re-parse document in background and produce requested range using absolute indices.
            match VgmDocument::try_from(data.as_slice()) {
                Ok(doc) => {
                    let total = doc.commands.len();
                    if absolute_start >= total {
                        // Nothing to do; send empty chunk. Use relative_start so the UI knows insertion pos.
                        let _ = tx.send(AstBuildMessage::Partial {
                            path,
                            start: relative_start,
                            nodes: Vec::new(),
                        });
                        return;
                    }
                    let end = std::cmp::min(absolute_start + count, total);

                    let mut nodes: Vec<AstNode> = Vec::with_capacity(end - absolute_start);
                    // Compute absolute offsets/lengths for commands once and attach them
                    // to the returned AstNodes so the UI can highlight the exact bytes.
                    let abs_ranges = doc.sourcemap();
                    for (abs_i, cmd) in doc.iter().enumerate().take(end).skip(absolute_start) {
                        let title = format!("{}: {:?}", abs_i, cmd);
                        let detail = format!("{:?}", cmd);
                        if let Some((off, len)) = abs_ranges.get(abs_i).copied() {
                            nodes.push(AstNode::new(title, detail).with_byte_range(off, len));
                        } else {
                            nodes.push(AstNode::new(title, detail));
                        }
                    }

                    let _ = tx.send(AstBuildMessage::Partial {
                        path,
                        start: relative_start,
                        nodes,
                    });
                }
                Err(e) => {
                    let _ = tx.send(AstBuildMessage::Error(format!("{:?}", e)));
                }
            }
        });
    }
}

/// Helper to build a path key string from a path Vec.
fn path_key_for(path: &[usize]) -> String {
    path.iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(".")
}

/// Parse an address/offset from an AstNode detail string.
///
/// Supports "0x..." hexadecimal tokens (first occurrence) and the first
/// contiguous decimal sequence otherwise. Returns `Some(offset)` on success.
fn parse_address_from_detail(detail: &str) -> Option<usize> {
    let s = detail.trim();

    // Try hex first: find "0x" and consume following hex digits.
    if let Some(pos) = s.find("0x") {
        let hex_str: String = s[pos + 2..]
            .chars()
            .take_while(|ch| ch.is_ascii_hexdigit())
            .collect();
        if !hex_str.is_empty() {
            // Try parsing and return the parsed value if successful; otherwise fall through
            // to decimal parsing below.
            if let Ok(v) = usize::from_str_radix(&hex_str, 16) {
                return Some(v);
            }
        }
    }

    // Fall back to the first contiguous decimal sequence (byte index).
    if let Some(pos) = s.find(|c: char| c.is_ascii_digit()) {
        let dec_str: String = s[pos..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if let Ok(v) = dec_str.parse::<usize>() {
            return Some(v);
        }
    }

    None
}

/// Draw an AstNode. Special handling if node.lazy_count.is_some(): we treat it as a
/// lazily-populated container and render only already-loaded children plus a
/// "Show more" button that requests the next chunk.
fn draw_ast_node(ui: &mut egui::Ui, node: &AstNode, path: Vec<usize>, state: &mut UiState) {
    use egui::CollapsingHeader;

    // Render only the first line of a node's title to avoid multi-line duplicate appearance.
    let display_title = node.title.lines().next().unwrap_or(&node.title).to_string();

    // If this is a lazy container (bucket) handle specially.
    if let Some(total) = node.lazy_count {
        // If this lazy node has a defined start index it represents a bucket range.
        if let Some(_start_idx) = node.lazy_start {
            CollapsingHeader::new(
                egui::RichText::new(&display_title).size(state.hex_viewer.font_size()),
            )
            .default_open(total <= 100)
            .show(ui, |ui| {
                ui.add_space(4.0);

                let key = path_key_for(&path);
                // Clone already-loaded children (if any) to avoid borrow issues.
                let children = state
                    .loaded_lazy_nodes
                    .get(&key)
                    .cloned()
                    .unwrap_or_default();
                if children.is_empty() {
                    // Not loaded yet — automatically request this bucket when the
                    // user opens the tree node. We avoid showing a button: the
                    // request is triggered once and a loading label is shown.
                    let pending = state.pending_requests.get(&key).copied().unwrap_or(false);
                    if pending {
                        ui.label("Loading...");
                    } else {
                        // Trigger background load for this bucket once.
                        // For bucketed lazy nodes we want to request the entire bucket
                        // (e.g. [0..1000]) so that expanding the bucket loads all entries
                        // rather than only a single chunk. Previously we used
                        // `lazy_chunk_size` here which caused only the first N items to load.
                        // NOTE: request_children expects `start` relative to the
                        // bucket, so pass 0 here (we want the full bucket).
                        let count = total;
                        // Defer the actual request to after drawing to avoid nested mutable borrows.
                        let key = path_key_for(&path);
                        if !state.enqueued_requests.contains_key(&key) {
                            state.deferred_loads.push((path.clone(), 0, count));
                            state.enqueued_requests.insert(key, true);
                        }
                        ui.label("Loading...");
                    }
                } else {
                    // Render loaded children for this bucket.
                    for (idx, child) in children.into_iter().enumerate() {
                        let mut child_path = path.clone();
                        child_path.push(idx);
                        draw_ast_node(ui, &child, child_path, state);
                    }
                }
            });
            return;
        } else {
            // Fallback generic lazy handling (should not be common with bucket approach).
            CollapsingHeader::new(
                egui::RichText::new(&display_title).size(state.hex_viewer.font_size()),
            )
            .default_open(total <= 100)
            .show(ui, |ui| {
                ui.add_space(4.0);
                let key = path_key_for(&path);
                let children = state
                    .loaded_lazy_nodes
                    .get(&key)
                    .cloned()
                    .unwrap_or_default();
                let loaded = children.len();
                for (idx, child) in children.into_iter().enumerate() {
                    let mut child_path = path.clone();
                    child_path.push(idx);
                    draw_ast_node(ui, &child, child_path, state);
                }

                if loaded < total {
                    let pending = state.pending_requests.get(&key).copied().unwrap_or(false);
                    let btn_label = format!("Show more ({}/{})", loaded, total);
                    if pending {
                        ui.label(btn_label);
                    } else if ui.button(btn_label).clicked() {
                        let start = loaded;
                        let count = state.lazy_chunk_size;
                        let key = path_key_for(&path);
                        if !state.enqueued_requests.contains_key(&key) {
                            state.deferred_loads.push((path.clone(), start, count));
                            state.enqueued_requests.insert(key, true);
                        }
                    }
                }
            });
            return;
        }
    }

    // Non-lazy node: render title only (no detail shown).
    if node.children.is_empty() {
        let selected = state
            .selected_ast
            .as_ref()
            .map(|p| *p == path)
            .unwrap_or(false);

        // Collapse repeated `path.len()` checks by evaluating the common prefix once.
        let label_str = if path.len() >= 2
            && (path[0] == 0
                || state
                    .ast_root
                    .get(path[0])
                    .map(|n| n.title == "GD3")
                    .unwrap_or(false))
        {
            // For header child items (top-level header is at path[0] == 0) and
            // GD3 child items (top-level node title == "GD3"), show the configured value inline.
            let detail_first = node.detail.lines().next().unwrap_or(&node.detail).trim();
            format!("{}: {}", display_title, detail_first)
        } else {
            display_title.clone()
        };
        // Truncate long labels for display while keeping the full `label_str` intact for copy operations.
        let display_label = {
            let max_chars = 120usize;
            if label_str.chars().count() > max_chars {
                let mut s = label_str.chars().take(max_chars).collect::<String>();
                s.push_str("...");
                s
            } else {
                label_str.clone()
            }
        };
        let title_text = egui::RichText::new(display_label).size(state.hex_viewer.font_size());
        // Use a SelectableLabel so the label is clickable and returns a Response.
        let response = ui.add(egui::SelectableLabel::new(selected, title_text.clone()));
        // Right-click context menu: allow copying the full (untruncated) label.
        // Call context_menu on a clone so we don't move `response`.
        response.clone().context_menu(|ui| {
            if ui.button("Copy").clicked() {
                // copy the full original (untruncated) label_str to the clipboard
                let label_clone = label_str.clone();
                ui.ctx().output_mut(|out| out.copied_text = label_clone);
                state.push_event(format!("copied: {}", &label_str));
                // Close the context menu after handling the click so it doesn't remain open.
                ui.close_menu();
            }
        });

        // If a keyboard-driven navigation requested that this path be focused/visible,
        // apply it now (focus). Keep the pending flag until we observe that the response
        // actually has focus so that we do not clear the request before the UI has applied the focus.
        if state.pending_focus.as_ref() == Some(&path) {
            // Request focus programmatically; do not clear pending_focus here.
            // Record the widget id we requested focus for so Tab-focus suppression can
            // re-apply it later if needed.
            state.last_focused_widget = Some(response.id);
            ui.ctx().memory_mut(|mem| mem.request_focus(response.id));
            // Do not scroll yet; wait until focus is observed to avoid premature clearing.
        }
        // Once the response actually has keyboard focus, consider the pending focus fulfilled
        // and perform the scroll-to-rect now so the left pane visibly follows keyboard navigation.
        if response.has_focus() && state.pending_focus.as_ref() == Some(&path) {
            // Scroll so this response rect is visible (centered).
            ui.scroll_to_rect(response.rect, Some(egui::Align::Center));
            // Clear the pending flag after performing the scroll.
            state.pending_focus = None;
        }

        // If this label currently has keyboard focus (e.g. arrived here via arrow keys)
        // treat it like a click so keyboard-only navigation immediately updates selection
        // and the hex viewer, without requiring Enter.
        if response.has_focus() && !selected {
            // Remember selected AST path
            state.selected_ast = Some(path.clone());
            // Remember the response rect so the left ScrollArea will scroll to it after drawing.
            state.last_selected_ast_rect = Some(response.rect);
            // Ensure an immediate repaint so the hex-viewer and pending-focus handling apply now.
            ui.ctx().request_repaint();

            // Clear previous hex highlights/markers and any overlay outlines
            state.hex_viewer.clear_selection_range();
            state.hex_viewer.clear_reference_markers();
            state.hex_viewer.clear_outline_ranges();
            // Default to drawing selection ranges with outlines unless overridden below.
            state.hex_viewer.set_selection_outline_enabled(true);

            // Prefer highlighting the full Header/GD3 top-level range when a child is selected.
            // If not applicable, fall back to the node's own byte_range or parsed address.
            let mut applied = false;
            if path.len() >= 2
                && let Some(top) = state.ast_root.get(path[0])
                && (top.title == "Header" || top.title == "GD3")
                && let Some((hs, hl)) = top.byte_range
                && hs < state.bytes.len()
                && hl > 0
            {
                let end = hs
                    .saturating_add(hl)
                    .saturating_sub(1)
                    .min(state.bytes.len().saturating_sub(1));
                // Highlight the entire header/gd3 as fill-only
                state.hex_viewer.set_selection_range(hs, end);
                state.hex_viewer.set_reference_markers(vec![hs]);
                state.hex_viewer.set_pending_scroll_to(hs, end);
                // Mark the parent header/gd3 range as fill-only so it draws without a stroke.
                state.hex_viewer.set_fill_only_ranges(vec![(hs, end)]);
                // For header contexts, use fill-only for the parent range (disable selection stroke).
                state.hex_viewer.set_selection_outline_enabled(false);
                // If this specific child has its own byte_range, draw it as an overlay outline.
                if let Some((cstart, clen)) = node.byte_range {
                    let cend = cstart.saturating_add(clen).saturating_sub(1);
                    state.hex_viewer.set_outline_ranges(vec![(cstart, cend)]);
                } else {
                    state.hex_viewer.clear_outline_ranges();
                }
                applied = true;
            }

            if !applied {
                // If top-level Header/GD3 range not applied, fall back to node.byte_range or parsed address.
                match node.byte_range {
                    Some((start, len)) if start < state.bytes.len() && len > 0 => {
                        let end = start
                            .saturating_add(len)
                            .saturating_sub(1)
                            .min(state.bytes.len().saturating_sub(1));
                        state.hex_viewer.set_selection_range(start, end);
                        state.hex_viewer.set_reference_markers(vec![start]);
                        state.hex_viewer.set_pending_scroll_to(start, end);
                    }
                    _ => {
                        // Try to parse an address/offset from the node.detail and highlight it,
                        // same logic as clicking the node (so keyboard-only selection updates the hex view).
                        if let Some(addr) = parse_address_from_detail(&node.detail)
                            .filter(|&a| a < state.bytes.len())
                        {
                            state.hex_viewer.set_selection_range(addr, addr);
                            state.hex_viewer.set_reference_markers(vec![addr]);
                            state.hex_viewer.set_pending_scroll_to(addr, addr);
                        }
                    }
                }
            }
        }

        if response.clicked() {
            // Remember selected AST path
            state.selected_ast = Some(path.clone());
            // Store the response rect so the outer ScrollArea can scroll to it after drawing.
            state.last_selected_ast_rect = Some(response.rect);

            // Give keyboard focus to this label so subsequent arrow keys are
            // received by the left pane and used for navigation.
            // Record the focused widget id so Tab suppression can restore it.
            state.last_focused_widget = Some(response.id);
            response.request_focus();

            // Clear previous hex highlights/markers and any overlay outlines
            state.hex_viewer.clear_selection_range();
            state.hex_viewer.clear_reference_markers();
            state.hex_viewer.clear_outline_ranges();
            // Default to drawing selection ranges with outlines unless overridden below.
            state.hex_viewer.set_selection_outline_enabled(true);

            // Prefer highlighting the full Header/GD3 top-level range when a child is clicked.
            let mut applied = false;
            if path.len() >= 2
                && let Some(top) = state.ast_root.get(path[0])
                && (top.title == "Header" || top.title == "GD3")
                && let Some((hs, hl)) = top.byte_range
                && hs < state.bytes.len()
                && hl > 0
            {
                let end = hs
                    .saturating_add(hl)
                    .saturating_sub(1)
                    .min(state.bytes.len().saturating_sub(1));
                // Highlight the entire header/gd3 as fill-only
                state.hex_viewer.set_selection_range(hs, end);
                state.hex_viewer.set_reference_markers(vec![hs]);
                // Also request auto-scroll so the selected range is brought into view.
                state.hex_viewer.set_pending_scroll_to(hs, end);
                // For header contexts, use fill-only for the parent range
                state.hex_viewer.set_selection_outline_enabled(false);
                // Draw an overlay outline for the specific child if available.
                if let Some((cstart, clen)) = node.byte_range {
                    let cend = cstart.saturating_add(clen).saturating_sub(1);
                    state.hex_viewer.set_outline_ranges(vec![(cstart, cend)]);
                } else {
                    state.hex_viewer.clear_outline_ranges();
                }
                applied = true;
            }

            if !applied {
                // If top-level Header/GD3 range not applied, fall back to node.byte_range or parsed address.
                match node.byte_range {
                    Some((start, len)) if start < state.bytes.len() && len > 0 => {
                        let end = start
                            .saturating_add(len)
                            .saturating_sub(1)
                            .min(state.bytes.len().saturating_sub(1));
                        state.hex_viewer.set_selection_range(start, end);
                        state.hex_viewer.set_reference_markers(vec![start]);
                        // Also request auto-scroll so the selected range is brought into view.
                        state.hex_viewer.set_pending_scroll_to(start, end);
                    }
                    _ => {
                        // Try to parse an address/offset from the node.detail and highlight it.
                        if let Some(addr) = parse_address_from_detail(&node.detail)
                            .filter(|&a| a < state.bytes.len())
                        {
                            // Highlight the byte and add a reference marker at that offset.
                            state.hex_viewer.set_selection_range(addr, addr);
                            state.hex_viewer.set_reference_markers(vec![addr]);
                            // Also request auto-scroll so the selected byte is brought into view.
                            state.hex_viewer.set_pending_scroll_to(addr, addr);
                        }
                    }
                }
            }
        }
    } else {
        CollapsingHeader::new(egui::RichText::new(&node.title).size(state.hex_viewer.font_size()))
            .default_open(false)
            .show(ui, |ui| {
                ui.add_space(4.0);
                for (i, child) in node.children.iter().enumerate() {
                    let mut child_path = path.clone();
                    child_path.push(i);
                    draw_ast_node(ui, child, child_path, state);
                }
            });
    }
}

/// Top-level UI entry called each frame.
pub fn show_ui(state: &mut UiState, ctx: &egui::Context, _frame: &mut eframe::Frame) {
    // If we have bytes but no AST yet, start initial populate.
    if state.ast_root.is_empty() && !state.bytes.is_empty() {
        let bytes_clone = state.bytes.clone();
        state.populate_from_bytes(&bytes_clone);
    }

    // Poll any background messages (drain all available messages).
    // To avoid borrow conflicts we first drain messages into a local Vec while
    // holding the receiver, then put the receiver back and process the messages
    // (which mutates `state`) afterwards.
    if let Some(rx) = state.ast_build_rx.take() {
        let mut msgs = Vec::new();
        let mut keep_rx = true;
        loop {
            match rx.try_recv() {
                Ok(msg) => {
                    msgs.push(msg);
                }
                Err(mpsc::TryRecvError::Empty) => {
                    // No more messages right now.
                    break;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    // Channel closed; do not put receiver back.
                    keep_rx = false;
                    break;
                }
            }
        }

        // Put the receiver back if it's still usable.
        if keep_rx {
            state.ast_build_rx = Some(rx);
        } else {
            state.ast_build_rx = None;
        }

        // Now process collected messages, mutating `state` as needed.
        for msg in msgs {
            match msg {
                AstBuildMessage::Full(nodes) => {
                    // Receive initial lightweight AST.
                    state.ast_root = nodes;
                    // Clear any previous lazy loads
                    state.loaded_lazy_nodes.clear();
                    state.pending_requests.clear();
                    state.ast_building = false;
                    state.push_event("received: full ast".to_string());
                }
                AstBuildMessage::Partial { path, start, nodes } => {
                    // Capture node count early because `nodes` may be moved below.
                    let nodes_count = nodes.len();

                    // Build the path key (no mutable borrow of `state` yet).
                    let path_key = path
                        .iter()
                        .map(|i| i.to_string())
                        .collect::<Vec<_>>()
                        .join(".");

                    // To avoid holding multiple mutable borrows of `state`,
                    // remove the existing entry from the map, operate on it locally,
                    // then re-insert the updated vector. This prevents nested
                    // mutable borrows when other state methods are called.
                    let mut entry = state
                        .loaded_lazy_nodes
                        .remove(&path_key)
                        .unwrap_or_default();

                    // If start matches current length, append; if start < len, try to splice in.
                    if start == entry.len() {
                        entry.extend(nodes);
                    } else if start < entry.len() {
                        // Overwrite existing range if overlapping (best-effort).
                        let mut idx = start;
                        for n in nodes.into_iter() {
                            if idx < entry.len() {
                                entry[idx] = n;
                            } else {
                                entry.push(n);
                            }
                            idx += 1;
                        }
                    } else {
                        // start > len: pad with placeholders (unlikely) then append.
                        let pad = start - entry.len();
                        for _ in 0..pad {
                            entry.push(AstNode::new("<placeholder>", ""));
                        }
                        entry.extend(nodes);
                    }

                    let new_len = entry.len();
                    // Re-insert the updated entry into the map.
                    state.loaded_lazy_nodes.insert(path_key.clone(), entry);

                    // Clear pending flag.
                    state.pending_requests.remove(&path_key);

                    // Push a compact event marker (no-op in release).
                    state.push_event(format!(
                        "recv partial: path={:?} start={} nodes={}",
                        path, start, nodes_count
                    ));
                    state.push_event(format!("inserted: {} now {} items", path_key, new_len));
                }
                AstBuildMessage::Diff(diffs, rebuilt_bytes) => {
                    // Receive diff ranges produced by the background parse + serialization.
                    // Update hex viewer overlay ranges so mismatches are shown as red outlines.
                    state.hex_viewer.set_diff_ranges(diffs);
                    // Provide the rebuilt bytes to the HexViewer so its diff tooltip can
                    // show both Original and Rebuilt values. Clone here because we will
                    // also store the rebuilt bytes in the UiState.
                    state
                        .hex_viewer
                        .set_rebuilt_bytes(Some(rebuilt_bytes.clone()));
                    // Store the rebuilt bytes on the UI state so other UI code can access them.
                    state.rebuilt_bytes = Some(rebuilt_bytes);
                    // Ensure the UI repaints immediately so the hex viewer processes any
                    // pending scroll requests (e.g. scroll to first diff) without waiting.
                    ctx.request_repaint();
                    state.push_event("received: diff ranges".to_string());
                }
                AstBuildMessage::Error(e) => {
                    state.ast_root = vec![AstNode::new("Parse Error", e)];
                    state.ast_building = false;
                    state.pending_requests.clear();
                    state.loaded_lazy_nodes.clear();
                    state.push_event("received: parse error".to_string());
                }
            }
        }
    }

    // Left sidebar AST
    egui::SidePanel::left("ast_panel")
        .resizable(false)
        // Reduce default left panel width so the hex viewer on the right is more visible.
        .default_width(240.0)
        // Keep the left panel width fixed so clicking inside doesn't cause the separator
        // to jump when internal content briefly changes size.
        .min_width(240.0)
        .max_width(240.0)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    // Add 8px top padding in the left pane.
                    ui.add_space(8.0);

                    // Clone the top-level AST into a snapshot to avoid holding an
                    // immutable borrow of `state.ast_root` while `draw_ast_node`
                    // may mutably borrow `state`. Cloning only the top-level
                    // nodes avoids borrow conflicts during recursive drawing.
                    let ast_snapshot = state.ast_root.clone();

                    // Keyboard navigation: Up/Down to move selection between top-level AST nodes.
                    // When selection changes, update the hex viewer selection similarly to a click.
                    // Use `ctx.input()` here so keyboard events are taken from the application
                    // context (not the local UI), which improves reliability for left-pane navigation.
                    let input = ctx.input(|i| i.clone());
                    let total = ast_snapshot.len();

                    // Tab pressed: schedule re-focus of currently selected AST path (strong suppression of Tab focus).
                    if input.key_pressed(egui::Key::Tab) {
                        // Use the currently selected AST path as the pending focus so Tab
                        // will not move focus to other UI elements. This strongly suppresses
                        // Tab-driven focus changes by re-asserting the selection as the target.
                        state.pending_focus = state.selected_ast.clone();
                        // Ensure the pending focus is applied promptly.
                        ctx.request_repaint();
                    }

                    // Diff navigation shortcuts: 'n' -> next, 'p' -> prev
                    if input.key_pressed(egui::Key::N) && state.hex_viewer.has_diffs() {
                        state.hex_viewer.next_diff();
                        ctx.request_repaint();
                    }
                    if input.key_pressed(egui::Key::P) && state.hex_viewer.has_diffs() {
                        state.hex_viewer.prev_diff();
                        ctx.request_repaint();
                    }

                    // Keyboard navigation for left-pane top-level selection (Up/Down only).
                    if (input.key_pressed(egui::Key::ArrowUp)
                        || input.key_pressed(egui::Key::ArrowDown))
                        && total > 0
                    {
                        let cur = state.selected_ast.as_ref().and_then(|p| p.first().copied());
                        let new_idx = if input.key_pressed(egui::Key::ArrowUp) {
                            match cur {
                                Some(0) => Some(0),
                                Some(n) => Some(n.saturating_sub(1)),
                                None => Some(total.saturating_sub(1)),
                            }
                        } else {
                            // ArrowDown
                            match cur {
                                Some(n) if n + 1 < total => Some(n + 1),
                                Some(_) => Some(total.saturating_sub(1)),
                                None => Some(0),
                            }
                        };
                        if let Some(idx) = new_idx {
                            // Select the top-level node at `idx`.
                            state.selected_ast = Some(vec![idx]);
                            // Ensure keyboard-driven selection will focus & scroll the corresponding label.
                            state.pending_focus = Some(vec![idx]);
                            // Force a repaint so the pending focus + hex-view updates are applied promptly.
                            ctx.request_repaint();

                            // Clear previous hex highlights/markers and overlays
                            state.hex_viewer.clear_selection_range();
                            state.hex_viewer.clear_reference_markers();
                            state.hex_viewer.clear_outline_ranges();
                            state.hex_viewer.set_selection_outline_enabled(true);

                            // If this node has an associated byte_range, apply it to the hex viewer.
                            // Otherwise, attempt to parse the node.detail similarly to a click,
                            // so keyboard-only navigation updates the hex view immediately.
                            if let Some(node) = state.ast_root.get(idx) {
                                match node.byte_range {
                                    Some((start, len)) if start < state.bytes.len() && len > 0 => {
                                        let end = start
                                            .saturating_add(len)
                                            .saturating_sub(1)
                                            .min(state.bytes.len().saturating_sub(1));
                                        state.hex_viewer.set_selection_range(start, end);
                                        state.hex_viewer.set_reference_markers(vec![start]);
                                        state.hex_viewer.set_pending_scroll_to(start, end);
                                    }
                                    _ => {
                                        if let Some(addr) = parse_address_from_detail(&node.detail)
                                            .filter(|&a| a < state.bytes.len())
                                        {
                                            state.hex_viewer.set_selection_range(addr, addr);
                                            state.hex_viewer.set_reference_markers(vec![addr]);
                                            state.hex_viewer.set_pending_scroll_to(addr, addr);
                                        }
                                    }
                                }
                            }
                        }
                    }

                    for (i, node) in ast_snapshot.iter().enumerate() {
                        draw_ast_node(ui, node, vec![i], state);
                    }
                    // If an AST node set a last_selected_ast_rect during drawing (keyboard-driven
                    // navigation or click), scroll the left panel so the selected node is visible.
                    if let Some(r) = state.last_selected_ast_rect.take() {
                        ui.scroll_to_rect(r, Some(egui::Align::Center));
                    }
                });

            ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                ui.add_space(8.0);
            });
        });

    // Right: hex viewer & toolbar
    egui::CentralPanel::default().show(ctx, |ui| {
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                // "Bytes" label removed from the right pane per request.
                if state.ast_building {
                    ui.add_space(12.0);
                    ui.colored_label(ui.visuals().selection.bg_fill, "Parsing...");
                }

                // Diff status indicator in the right-pane toolbar:
                // - If diffs exist: show a red message with count.
                // - If no diffs and bytes are loaded: show a green message clarifying
                //   that original bytes equal re-serialized (rebuilt) bytes.
                ui.add_space(8.0);
                let _has_diffs = state.hex_viewer.has_diffs();
                // Center Prev/Next and Diff X/Y together in the toolbar.
                {
                    // Visual/button size (approximate)
                    let btn_size = egui::vec2(72.0, 26.0);
                    let font_size_btn = state.hex_viewer.font_size();
                    let has_diffs = state.hex_viewer.has_diffs();

                    // Prepare the diff text and estimate its width so we can center the whole group.
                    let total = state.hex_viewer.diff_ranges().len();
                    let cur_disp = match state.hex_viewer.current_diff_index() {
                        Some(c) if total > 0 => c + 1,
                        _ => 0,
                    };
                    let diff_text = format!("Diff {}/{}", cur_disp, total);

                    // Heuristic char width (monospace-like)
                    let char_w = font_size_btn * 0.6_f32;
                    let diff_w = (diff_text.len() as f32) * char_w + 12.0;

                    // Spacing used between elements (matches previous gaps used).
                    let gap_prev_next = 8.0_f32;
                    let gap_next_diff = 12.0_f32;

                    // Compute the group's total width: prev button + gap + next button + gap + diff label
                    let group_w = btn_size.x + gap_prev_next + btn_size.x + gap_next_diff + diff_w;

                    // Available width to center within
                    let avail_w = ui.available_width();
                    let left_space = ((avail_w - group_w) / 2.0).max(0.0);

                    // Add left spacer so the group appears centered in the toolbar area.
                    ui.add_space(left_space);

                    // Helper to draw toolbar-style mouse-only button (enabled/disabled visual).
                    let draw_toolbar_btn = |ui: &mut egui::Ui, label: &str, enabled: bool| {
                        let (rect, resp) = ui.allocate_exact_size(btn_size, egui::Sense::click());
                        let painter = ui.painter_at(rect);

                        let hovered = enabled && resp.hovered();
                        let pressed = enabled
                            && (resp.clicked() || (hovered && ui.input(|i| i.pointer.any_down())));

                        // Subtle shadow for depth.
                        let shadow_col = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 40);
                        painter.rect_filled(rect.translate(egui::vec2(0.0, 2.0)), 6.0, shadow_col);

                        // Background based on enabled/hover/pressed.
                        let bg_fill = if !enabled {
                            ui.visuals().widgets.inactive.bg_fill
                        } else if pressed {
                            ui.visuals().widgets.active.bg_fill
                        } else if hovered {
                            ui.visuals().widgets.hovered.bg_fill
                        } else {
                            ui.visuals().widgets.inactive.bg_fill
                        };
                        painter.rect_filled(rect, 6.0, bg_fill);

                        if hovered {
                            let rim = ui.visuals().widgets.hovered.fg_stroke.color;
                            painter.rect_stroke(rect.shrink(1.0), 6.0, egui::Stroke::new(1.0, rim));
                        }

                        // Text with slight offset when pressed; dim when disabled.
                        let text_offset_y = if pressed { 1.6 } else { 0.0 };
                        let mut text_col = ui.visuals().text_color();
                        if !enabled {
                            text_col = egui::Color32::from_rgba_unmultiplied(
                                text_col.r(),
                                text_col.g(),
                                text_col.b(),
                                120,
                            );
                        }
                        painter.text(
                            egui::pos2(rect.center().x, rect.center().y + text_offset_y),
                            egui::Align2::CENTER_CENTER,
                            label,
                            egui::FontId::proportional(font_size_btn),
                            text_col,
                        );

                        resp
                    };

                    // Render the group horizontally: Prev, gap, Next, gap, Diff label.
                    ui.horizontal(|ui| {
                        // Prev
                        let prev_resp = draw_toolbar_btn(ui, "Prev", has_diffs);
                        if has_diffs && prev_resp.clicked() {
                            state.hex_viewer.prev_diff();
                            ctx.request_repaint();
                        }

                        ui.add_space(gap_prev_next);

                        // Next
                        let next_resp = draw_toolbar_btn(ui, "Next", has_diffs);
                        if has_diffs && next_resp.clicked() {
                            state.hex_viewer.next_diff();
                            ctx.request_repaint();
                        }

                        ui.add_space(gap_next_diff);

                        // Diff label
                        ui.label(diff_text);
                    });
                }
            });

            ui.add_space(6.0);

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    // Ensure the HexViewer always has access to the ORIGINAL file bytes so
                    // its diff tooltip can display the true Original values even when the
                    // viewer is asked to render the rebuilt bytes.
                    state
                        .hex_viewer
                        .set_original_bytes(Some(state.bytes.clone()));

                    // Prefer showing the rebuilt/serialized bytes in the right pane when available.
                    // The background parse/serializer supplies `rebuilt_bytes` via AstBuildMessage::Diff.
                    if let Some(rb) = state.rebuilt_bytes.as_ref() {
                        state.hex_viewer.show(ui, rb);
                    } else {
                        state.hex_viewer.show(ui, &state.bytes);
                    }
                });
        });
    });

    // If the HexViewer recorded a byte click, consume it here and focus the corresponding
    // AST node in the left pane (if a mapping exists). The HexViewer now exposes the last
    // clicked byte via `take_last_clicked_byte()` so we avoid using temporary egui storage.
    //
    // We consume the clicked index (if any), locate an AST node whose byte_range covers the clicked
    // offset (searching top-level AST nodes first, then any loaded lazy children), and then
    // set `state.selected_ast` / `state.pending_focus` so the left pane focuses that node.
    if let Some(clicked) = state.hex_viewer.take_last_clicked_byte() {
        let mut found_path: Option<Vec<usize>> = None;

        // 1) Check top-level AST nodes (e.g., Header, Commands, GD3) for a byte_range that covers the click.
        for (i, node) in state.ast_root.iter().enumerate() {
            if let Some((s, len)) = node.byte_range {
                let e = s.saturating_add(len).saturating_sub(1);
                if clicked >= s && clicked <= e {
                    found_path = Some(vec![i]);
                    break;
                }
            }
        }

        // 2) If not found, search loaded lazy nodes (buckets) where each entry contains command AstNodes
        //    with their own byte_range. The loaded_lazy_nodes keys are path strings like "1.0".
        if found_path.is_none() {
            'outer: for (key, nodes) in state.loaded_lazy_nodes.iter() {
                // Parse key into a path Vec<usize> (e.g. "1.0" -> vec![1,0])
                let base_path: Vec<usize> = if key.is_empty() {
                    Vec::new()
                } else {
                    key.split('.')
                        .filter_map(|s| s.parse::<usize>().ok())
                        .collect::<Vec<usize>>()
                };

                for (idx, n) in nodes.iter().enumerate() {
                    if let Some((s, len)) = n.byte_range {
                        let e = s.saturating_add(len).saturating_sub(1);
                        if clicked >= s && clicked <= e {
                            let mut full_path = base_path.clone();
                            full_path.push(idx);
                            found_path = Some(full_path);
                            break 'outer;
                        }
                    }
                }
            }
        }

        // If we found a matching path, set selection + pending_focus so the left pane
        // highlights and scrolls to the matching command/node. Also update hex highlights.
        if let Some(path) = found_path {
            state.selected_ast = Some(path.clone());
            state.pending_focus = Some(path.clone());
            // Also update hex viewer selection and markers to reflect the clicked byte.
            state.hex_viewer.clear_selection_range();
            state.hex_viewer.set_selection_range(clicked, clicked);
            state.hex_viewer.set_reference_markers(vec![clicked]);
            state.hex_viewer.set_pending_scroll_to(clicked, clicked);
            // Repaint to ensure the left pane observes focus change promptly.
            ctx.request_repaint();
        }
    }

    // Drain deferred loads queued during drawing to avoid nested mutable borrows.
    if !state.deferred_loads.is_empty() {
        let mut to_process = Vec::new();
        std::mem::swap(&mut to_process, &mut state.deferred_loads);
        for (path, start, count) in to_process {
            let key = path_key_for(&path);
            // Remove enqueued marker so request_children can set pending_requests and proceed.
            state.enqueued_requests.remove(&key);
            state.request_children(path, start, count);
        }
    }
}
