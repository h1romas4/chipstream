use super::ast::{build_raw_binary_nodes, source_node_to_ast};
use super::{AstBuildMessage, AstNode};
use crate::sourcemap::{MdxAdapter, PdxAdapter, SourceAdapter, VgmAdapter};
use std::cmp;
use std::path::Path;
use std::sync::mpsc;
use std::thread;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InputFormat {
    Auto,
    Pdx,
}

impl InputFormat {
    pub(crate) fn from_file_name(file_name: Option<&str>) -> Self {
        file_name.map_or(Self::Auto, |name| Self::from_path(Path::new(name)))
    }

    pub(crate) fn from_path(path: &Path) -> Self {
        if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pdx"))
        {
            Self::Pdx
        } else {
            Self::Auto
        }
    }
}

pub(crate) fn spawn_initial_parse(
    data: Vec<u8>,
    generation: u64,
    tx: mpsc::Sender<AstBuildMessage>,
    input_format: InputFormat,
) {
    thread::spawn(move || {
        if input_format == InputFormat::Pdx {
            let nodes = match PdxAdapter::parse(&data) {
                Ok(document) => vec![source_node_to_ast(PdxAdapter::root_node(&document))],
                Err(error) => build_raw_binary_nodes(&data, &format!("PDX: {error}")),
            };
            let _ = tx.send(AstBuildMessage::Full { generation, nodes });
            return;
        }

        match parse_with_adapter::<VgmAdapter>(&data) {
            Ok(document) => {
                let mut nodes = vec![source_node_to_ast(VgmAdapter::header_node(&document))];
                let total_commands = document.commands.len();
                let bucket_size = 1000usize;
                let mut buckets = Vec::new();
                let mut start = 0usize;
                while start < total_commands {
                    let end = cmp::min(start + bucket_size, total_commands);
                    buckets.push(
                        AstNode::new(
                            format!("[{start}..{end}]"),
                            format!("{} commands", end - start),
                        )
                        .with_lazy_range(start, end - start),
                    );
                    start = end;
                }
                nodes.push(
                    AstNode::new("Commands", format!("{total_commands} commands"))
                        .with_children(buckets),
                );
                if let Some(gd3_node) = VgmAdapter::gd3_node(&document).map(source_node_to_ast) {
                    nodes.push(gd3_node);
                }
                let rebuilt_bytes = canonical_bytes_with_adapter::<VgmAdapter>(&document);
                let diffs = compute_diff_ranges(&data, &rebuilt_bytes);
                let _ = tx.send(AstBuildMessage::Full { generation, nodes });
                let _ = tx.send(AstBuildMessage::Diff {
                    generation,
                    diffs,
                    rebuilt_bytes,
                });
            }
            Err(vgm_error) => match MdxAdapter::parse(&data) {
                Ok(document) => {
                    let space = MdxAdapter::coordinate_space(&data);
                    let mut nodes = vec![source_node_to_ast(MdxAdapter::header_node(
                        &document, space,
                    ))];
                    if let Some(tone_node) = MdxAdapter::tone_node(&document, space) {
                        nodes.push(source_node_to_ast(tone_node));
                    }
                    nodes.extend(
                        document
                            .tracks
                            .iter()
                            .enumerate()
                            .filter(|(_, commands)| !commands.is_empty())
                            .map(|(track, commands)| {
                                AstNode::new(
                                    format!("Track {track}"),
                                    format!("{} commands", commands.len()),
                                )
                                .with_lazy_range(0, commands.len())
                                .with_lazy_track(track)
                            }),
                    );
                    let _ = tx.send(AstBuildMessage::Full { generation, nodes });
                }
                Err(mdx_error) => {
                    let parse_error = format!("VGM: {vgm_error:?}; MDX: {mdx_error:?}");
                    let _ = tx.send(AstBuildMessage::Full {
                        generation,
                        nodes: build_raw_binary_nodes(&data, &parse_error),
                    });
                }
            },
        }
    });
}

pub(crate) fn compute_diff_ranges(original: &[u8], rebuilt: &[u8]) -> Vec<(usize, usize)> {
    let max_len = original.len().max(rebuilt.len());
    let mut diffs = Vec::new();
    let mut diff_start = None;
    for index in 0..max_len {
        let differs = original.get(index) != rebuilt.get(index);
        match (diff_start, differs) {
            (None, true) => diff_start = Some(index),
            (Some(start), false) => {
                diffs.push((start, index - 1));
                diff_start = None;
            }
            _ => {}
        }
    }
    if let Some(start) = diff_start {
        diffs.push((start, max_len - 1));
    }
    diffs
}

pub(crate) struct ChildParseRequest {
    pub(crate) data: Vec<u8>,
    pub(crate) generation: u64,
    pub(crate) tx: mpsc::Sender<AstBuildMessage>,
    pub(crate) path: Vec<usize>,
    pub(crate) relative_start: usize,
    pub(crate) absolute_start: usize,
    pub(crate) count: usize,
    pub(crate) mdx_track: Option<usize>,
}

pub(crate) fn spawn_children_parse(request: ChildParseRequest) {
    let ChildParseRequest {
        data,
        generation,
        tx,
        path,
        relative_start,
        absolute_start,
        count,
        mdx_track,
    } = request;
    thread::spawn(move || {
        if let Some(track) = mdx_track {
            match parse_with_adapter::<MdxAdapter>(&data) {
                Ok(document) => {
                    let total = document.tracks.get(track).map_or(0, Vec::len);
                    let nodes = if absolute_start >= total {
                        Vec::new()
                    } else {
                        let end = cmp::min(absolute_start + count, total);
                        MdxAdapter::track_nodes_in_space(
                            &document,
                            track,
                            MdxAdapter::coordinate_space(&data),
                        )[absolute_start..end]
                            .iter()
                            .cloned()
                            .map(source_node_to_ast)
                            .collect()
                    };
                    let _ = tx.send(AstBuildMessage::Partial {
                        generation,
                        path,
                        start: relative_start,
                        nodes,
                    });
                }
                Err(error) => {
                    let _ = tx.send(AstBuildMessage::Error {
                        generation,
                        message: format!("{error:?}"),
                    });
                }
            }
            return;
        }

        match parse_with_adapter::<VgmAdapter>(&data) {
            Ok(document) => {
                let total = document.commands.len();
                if absolute_start >= total {
                    let _ = tx.send(AstBuildMessage::Partial {
                        generation,
                        path,
                        start: relative_start,
                        nodes: Vec::new(),
                    });
                    return;
                }
                let end = cmp::min(absolute_start + count, total);
                let nodes =
                    VgmAdapter::command_nodes(&document, absolute_start, end - absolute_start)
                        .into_iter()
                        .map(source_node_to_ast)
                        .collect();
                let _ = tx.send(AstBuildMessage::Partial {
                    generation,
                    path,
                    start: relative_start,
                    nodes,
                });
            }
            Err(error) => {
                let _ = tx.send(AstBuildMessage::Error {
                    generation,
                    message: format!("{error:?}"),
                });
            }
        }
    });
}

fn parse_with_adapter<A: SourceAdapter>(bytes: &[u8]) -> anyhow::Result<A::Document> {
    A::parse(bytes)
}

fn canonical_bytes_with_adapter<A: SourceAdapter>(document: &A::Document) -> Vec<u8> {
    A::canonical_bytes(document)
}

#[cfg(test)]
mod tests {
    use super::{ChildParseRequest, InputFormat, spawn_children_parse, spawn_initial_parse};
    use crate::gui::AstBuildMessage;
    use soundlog::VgmBuilder;
    use soundlog::mdx::command::MdxRest;
    use soundlog::mdx::document::MdxBuilder;
    use soundlog::mdx::pdx::PdxBuilder;
    use soundlog::vgm::command::WaitSamples;
    use std::path::Path;
    use std::sync::mpsc;
    use std::time::Duration;

    fn sample_vgm_bytes() -> Vec<u8> {
        let mut builder = VgmBuilder::new();
        builder.add_vgm_command(WaitSamples(735));
        let document = builder.finalize();
        (&document).into()
    }

    fn sample_mdx_bytes() -> Vec<u8> {
        let mut builder = MdxBuilder::new();
        builder.add_mdx_command(0, MdxRest::new(12).unwrap());
        builder.finalize().unwrap().to_bytes()
    }

    fn sample_pdx_bytes() -> Vec<u8> {
        let mut builder = PdxBuilder::new();
        builder.set_sample(0, 0, b"PCM1".to_vec()).unwrap();
        builder.finalize().to_bytes()
    }

    fn receive_message(receiver: mpsc::Receiver<AstBuildMessage>) -> AstBuildMessage {
        receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("lazy worker did not send a message")
    }

    #[test]
    fn input_format_detects_pdx_extension_case_insensitively() {
        assert_eq!(
            InputFormat::from_path(Path::new("music.PDX")),
            InputFormat::Pdx
        );
        assert_eq!(
            InputFormat::from_path(Path::new("music.mdx")),
            InputFormat::Auto
        );
        assert_eq!(
            InputFormat::from_file_name(Some("sample.pdx")),
            InputFormat::Pdx
        );
        assert_eq!(InputFormat::from_file_name(None), InputFormat::Auto);
    }

    #[test]
    fn pdx_worker_emits_source_tree_nodes() {
        let (tx, rx) = mpsc::channel();
        spawn_initial_parse(sample_pdx_bytes(), 15, tx, InputFormat::Pdx);

        match receive_message(rx) {
            AstBuildMessage::Full { generation, nodes } => {
                assert_eq!(generation, 15);
                assert_eq!(nodes.len(), 1);
                assert_eq!(nodes[0].title, "PDX");
                assert_eq!(nodes[0].children[0].title, "Bank 0");
                assert_eq!(nodes[0].children[0].children[0].title, "Note 0");
            }
            AstBuildMessage::Error { message, .. } => panic!("unexpected worker error: {message}"),
            _ => panic!("expected a Full message"),
        }
    }

    #[test]
    fn pdx_worker_emits_source_tree_for_compressed_input() {
        let mut builder = PdxBuilder::new();
        builder.set_sample(0, 0, b"PCM1".to_vec()).unwrap();
        builder.set_lz_compressed(true);
        let (tx, rx) = mpsc::channel();
        spawn_initial_parse(builder.finalize().to_bytes(), 16, tx, InputFormat::Pdx);

        match receive_message(rx) {
            AstBuildMessage::Full { nodes, .. } => {
                assert_eq!(nodes[0].title, "PDX");
                assert_eq!(nodes[0].children[0].title, "Bank 0");
                assert_eq!(nodes[0].children[0].children[0].title, "Note 0");
                assert_eq!(
                    nodes[0].mapped_range.unwrap().space,
                    crate::sourcemap::ByteCoordinateSpace::Logical
                );
                assert!(nodes[0].byte_range.is_none());
            }
            AstBuildMessage::Error { message, .. } => panic!("unexpected worker error: {message}"),
            _ => panic!("expected a Full message"),
        }
    }

    #[test]
    fn vgm_worker_emits_command_nodes_with_generation_and_path() {
        let (tx, rx) = mpsc::channel();
        spawn_children_parse(ChildParseRequest {
            data: sample_vgm_bytes(),
            generation: 11,
            tx,
            path: vec![1, 0],
            relative_start: 3,
            absolute_start: 0,
            count: 1,
            mdx_track: None,
        });

        match receive_message(rx) {
            AstBuildMessage::Partial {
                generation,
                path,
                start,
                nodes,
            } => {
                assert_eq!(generation, 11);
                assert_eq!(path, vec![1, 0]);
                assert_eq!(start, 3);
                assert_eq!(nodes.len(), 1);
                assert!(nodes[0].title.contains("Wait"));
                assert!(nodes[0].byte_range.is_some());
            }
            AstBuildMessage::Error { message, .. } => panic!("unexpected worker error: {message}"),
            _ => panic!("expected a Partial message"),
        }
    }

    #[test]
    fn mdx_worker_emits_track_nodes_with_generation_and_path() {
        let (tx, rx) = mpsc::channel();
        spawn_children_parse(ChildParseRequest {
            data: sample_mdx_bytes(),
            generation: 12,
            tx,
            path: vec![2],
            relative_start: 0,
            absolute_start: 0,
            count: 1,
            mdx_track: Some(0),
        });

        match receive_message(rx) {
            AstBuildMessage::Partial {
                generation,
                path,
                nodes,
                ..
            } => {
                assert_eq!(generation, 12);
                assert_eq!(path, vec![2]);
                assert_eq!(nodes.len(), 1);
                assert!(nodes[0].title.contains("Rest"));
                assert!(nodes[0].byte_range.is_some());
            }
            AstBuildMessage::Error { message, .. } => panic!("unexpected worker error: {message}"),
            _ => panic!("expected a Partial message"),
        }
    }

    #[test]
    fn worker_returns_empty_partial_for_out_of_range_start() {
        let (tx, rx) = mpsc::channel();
        spawn_children_parse(ChildParseRequest {
            data: sample_vgm_bytes(),
            generation: 13,
            tx,
            path: vec![1],
            relative_start: 7,
            absolute_start: usize::MAX,
            count: 4,
            mdx_track: None,
        });

        match receive_message(rx) {
            AstBuildMessage::Partial {
                generation,
                path,
                start,
                nodes,
            } => {
                assert_eq!(generation, 13);
                assert_eq!(path, vec![1]);
                assert_eq!(start, 7);
                assert!(nodes.is_empty());
            }
            _ => panic!("expected an empty Partial message"),
        }
    }

    #[test]
    fn worker_emits_error_for_unparseable_data() {
        let (tx, rx) = mpsc::channel();
        spawn_children_parse(ChildParseRequest {
            data: vec![0, 1, 2],
            generation: 14,
            tx,
            path: vec![0],
            relative_start: 0,
            absolute_start: 0,
            count: 1,
            mdx_track: None,
        });

        match receive_message(rx) {
            AstBuildMessage::Error {
                generation,
                message,
            } => {
                assert_eq!(generation, 14);
                assert!(!message.is_empty());
            }
            _ => panic!("expected an Error message"),
        }
    }
}
