use super::ast::{build_raw_binary_nodes, source_node_to_ast};
use super::{AstBuildMessage, AstNode};
use crate::sourcemap::{MdxAdapter, SourceAdapter, VgmAdapter};
use std::cmp;
use std::sync::mpsc;
use std::thread;

pub(crate) fn spawn_initial_parse(
    data: Vec<u8>,
    generation: u64,
    tx: mpsc::Sender<AstBuildMessage>,
) {
    thread::spawn(move || match parse_with_adapter::<VgmAdapter>(&data) {
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
                nodes.extend(document.tracks.iter().enumerate().map(|(track, commands)| {
                    let node = AstNode::new(
                        format!("Track {track}"),
                        format!("{} commands", commands.len()),
                    );
                    if commands.is_empty() {
                        node
                    } else {
                        node.with_lazy_range(0, commands.len())
                            .with_lazy_track(track)
                    }
                }));
                let rebuilt_bytes = canonical_bytes_with_adapter::<MdxAdapter>(&document);
                let diffs = compute_diff_ranges(&data, &rebuilt_bytes);
                let _ = tx.send(AstBuildMessage::Full { generation, nodes });
                let _ = tx.send(AstBuildMessage::Diff {
                    generation,
                    diffs,
                    rebuilt_bytes,
                });
            }
            Err(mdx_error) => {
                let parse_error = format!("VGM: {vgm_error:?}; MDX: {mdx_error:?}");
                let _ = tx.send(AstBuildMessage::Full {
                    generation,
                    nodes: build_raw_binary_nodes(&data, &parse_error),
                });
            }
        },
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

pub(crate) fn spawn_children_parse(
    data: Vec<u8>,
    generation: u64,
    tx: mpsc::Sender<AstBuildMessage>,
    path: Vec<usize>,
    relative_start: usize,
    absolute_start: usize,
    count: usize,
    mdx_track: Option<usize>,
) {
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
    use super::spawn_children_parse;
    use crate::gui::AstBuildMessage;
    use soundlog::VgmBuilder;
    use soundlog::mdx::command::MdxRest;
    use soundlog::mdx::document::MdxBuilder;
    use soundlog::vgm::command::WaitSamples;
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

    fn receive_message(receiver: mpsc::Receiver<AstBuildMessage>) -> AstBuildMessage {
        receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("lazy worker did not send a message")
    }

    #[test]
    fn vgm_worker_emits_command_nodes_with_generation_and_path() {
        let (tx, rx) = mpsc::channel();
        spawn_children_parse(sample_vgm_bytes(), 11, tx, vec![1, 0], 3, 0, 1, None);

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
        spawn_children_parse(sample_mdx_bytes(), 12, tx, vec![2], 0, 0, 1, Some(0));

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
        spawn_children_parse(sample_vgm_bytes(), 13, tx, vec![1], 7, usize::MAX, 4, None);

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
        spawn_children_parse(vec![0, 1, 2], 14, tx, vec![0], 0, 0, 1, None);

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
