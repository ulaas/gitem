//! The visual merge tool: synchronised Ours / Result / Theirs panes (optionally
//! Base), per-conflict resolutions, inline editing, and auto-merged regions.
use std::path::PathBuf;

use egui::{Color32, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use gitem_core::RepoOp;
use gitem_core::merge::{MergeDoc, Region, Resolution, has_conflict_markers};
use gitem_core::{ops, util};

use crate::action::Cx;
use crate::task::Task;
use crate::theme::Palette;
use crate::widgets::*;

const COLLAPSE_CONTEXT: usize = 3;

pub enum MergeSource {
    /// A conflicted path in the current repository (index stages 1, 2, 3).
    Repo(String),
    /// Standalone mergetool mode with explicit files.
    Files { base: PathBuf, local: PathBuf, remote: PathBuf, merged: PathBuf },
}

struct Loaded {
    doc: MergeDoc,
    binary: bool,
    missing_ours: bool,
    missing_theirs: bool,
    /// Which versions of the file this was loaded from.
    signature: String,
}

/// Bytes as text, only if nothing would be altered by treating them so. Lossy
/// decoding would write replacement characters into the file on save.
fn exact_text(b: &[u8]) -> Option<&str> {
    if util::is_binary(b) { None } else { std::str::from_utf8(b).ok() }
}

fn build_doc(b: Vec<u8>, o: Vec<u8>, t: Vec<u8>, signature: String) -> Loaded {
    let (mo, mt) = (o.is_empty(), t.is_empty());
    match (exact_text(&b), exact_text(&o), exact_text(&t)) {
        (Some(bt), Some(ot), Some(tt)) => Loaded { doc: MergeDoc::new(bt, ot, tt), binary: false, missing_ours: mo, missing_theirs: mt, signature },
        _ => Loaded { doc: MergeDoc::default(), binary: true, missing_ours: mo, missing_theirs: mt, signature },
    }
}

pub struct MergeView {
    pub source: MergeSource,
    doc: Task<Loaded>,
    started: bool,
    show_base: bool,
    current: usize,
    scroll_to: Option<usize>,
    editing: Option<(usize, String)>,
    raw_mode: bool,
    raw_text: String,
    expanded: Vec<usize>,
    saved: bool,
    pub finished: Option<bool>,
    check: Task<String>,
    checked_gen: u64,
    /// The file is no longer in conflict, so saving here would overwrite newer work.
    resolved: bool,
    /// What the two sides are called. A rebase replays your commits onto the
    /// upstream, so there "ours" is the upstream and "theirs" is your own work.
    sides: (&'static str, &'static str),
    ai: Task<Vec<(usize, Vec<String>)>>,
    /// Testing aid: resolve everything to theirs and save without a click.
    pub auto_accept: bool,
}

impl MergeView {
    pub fn for_repo_path(path: &str) -> Self {
        Self::with_source(MergeSource::Repo(path.to_string()))
    }

    pub fn with_source(source: MergeSource) -> Self {
        Self {
            source,
            doc: Task::default(),
            started: false,
            show_base: false,
            current: 0,
            scroll_to: None,
            editing: None,
            raw_mode: false,
            raw_text: String::new(),
            expanded: Vec::new(),
            saved: false,
            finished: None,
            check: Task::default(),
            checked_gen: 0,
            resolved: false,
            sides: ("ours", "theirs"),
            ai: Task::default(),
            auto_accept: false,
        }
    }

    pub fn key(&self) -> String {
        match &self.source {
            MergeSource::Repo(p) => format!("merge:{p}"),
            MergeSource::Files { merged, .. } => format!("merge:{}", merged.display()),
        }
    }

    pub fn title(&self) -> String {
        let p = match &self.source {
            MergeSource::Repo(p) => p.clone(),
            MergeSource::Files { merged, .. } => merged.display().to_string(),
        };
        format!("Merge {}", p.rsplit(['/', '\\']).next().unwrap_or(&p))
    }

    fn start(&mut self, ui: &Ui, cx: &Cx) {
        self.started = true;
        let repo = cx.repo.clone();
        match &self.source {
            MergeSource::Repo(p) => {
                let p = p.clone();
                self.doc.spawn(ui.ctx(), move || -> Result<Loaded, String> {
                    Ok(build_doc(repo.stage_content(1, &p), repo.stage_content(2, &p), repo.stage_content(3, &p), repo.unmerged_signature(&p)))
                });
            }
            MergeSource::Files { base, local, remote, .. } => {
                let (b, l, r) = (base.clone(), local.clone(), remote.clone());
                self.doc.spawn(ui.ctx(), move || -> Result<Loaded, String> {
                    let rd = |p: &PathBuf| std::fs::read(p).unwrap_or_default();
                    Ok(build_doc(rd(&b), rd(&l), rd(&r), String::new()))
                });
            }
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let pal = cx.pal.clone();
        self.doc.poll();
        if !self.started {
            self.start(ui, cx);
        }
        self.track_conflict(ui, cx);
        self.sides = if cx.status().is_some_and(|s| matches!(s.op, RepoOp::Rebase | RepoOp::RebaseInteractive)) { ("upstream", "your commit") } else { ("ours", "theirs") };
        if let Some(e) = &self.doc.error {
            error_box(ui, e, &pal);
            return;
        }
        let Some(mut loaded) = self.doc.value.take() else {
            spinner_row(ui, "Loading the three versions…", &pal);
            return;
        };
        if self.resolved {
            ui.add_space(12.0);
            chip(ui, "✔ This file is no longer in conflict", pal.good);
            ui.label(RichText::new("You can close this tab. It updates by itself if the file conflicts again.").color(pal.dim));
            self.doc.value = Some(loaded);
            return;
        }
        if (loaded.binary || loaded.missing_ours || loaded.missing_theirs)
            && let MergeSource::Repo(path) = &self.source {
                let path = path.clone();
                self.structural(ui, cx, &path, &loaded);
                self.doc.value = Some(loaded);
                return;
            }
        if loaded.binary {
            self.structural_files(ui, cx);
            self.doc.value = Some(loaded);
            return;
        }
        if self.auto_accept {
            loaded.doc.resolve_all(Resolution::Theirs);
            self.save(cx, &loaded.doc, true);
        }
        self.toolbar(ui, cx, &mut loaded.doc);
        ui.separator();
        if self.raw_mode {
            egui::ScrollArea::both().id_salt("merge-raw").auto_shrink([false, false]).show(ui, |ui| {
                ui.add(crate::widgets::text_area(&mut self.raw_text).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY).code_editor());
            });
        } else {
            self.panes(ui, cx, &mut loaded.doc);
        }
        self.doc.value = Some(loaded);
    }

    /// Notice when git moves on: the file gets resolved, or a later rebase step
    /// conflicts on it again with different versions, which must replace these.
    fn track_conflict(&mut self, ui: &Ui, cx: &Cx) {
        let MergeSource::Repo(path) = &self.source else { return };
        if self.checked_gen != cx.data.status_gen && !self.check.busy() {
            self.checked_gen = cx.data.status_gen;
            let (r, p) = (cx.repo.clone(), path.clone());
            self.check.spawn(ui.ctx(), move || -> Result<String, String> { Ok(r.unmerged_signature(&p)) });
        }
        if self.check.poll() {
            let now = self.check.value.take().unwrap_or_default();
            self.resolved = now.is_empty();
            let stale = self.doc.get().is_some_and(|l| l.signature != now);
            if !now.is_empty() && stale {
                self.started = false;
                self.doc.value = None;
                self.editing = None;
                self.expanded.clear();
                self.raw_mode = false;
                self.saved = false;
                self.current = 0;
            }
        }
    }

    /// Standalone mergetool on content that can't be merged as text.
    fn structural_files(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let pal = cx.pal.clone();
        let MergeSource::Files { local, remote, merged, .. } = &self.source else { return };
        ui.add_space(12.0);
        ui.label(RichText::new("This file is binary or isn't UTF-8 text, so it can't be merged line by line. Choose which version to keep.").color(pal.dim));
        ui.add_space(8.0);
        let mut picked = None;
        ui.horizontal(|ui| {
            if ui.button("Keep local").clicked() {
                picked = Some(local.clone());
            }
            if ui.button("Keep remote").clicked() {
                picked = Some(remote.clone());
            }
            if ui.button("Cancel merge").clicked() {
                self.finished = Some(false);
            }
        });
        if let Some(src) = picked {
            match std::fs::copy(&src, merged) {
                Ok(_) => self.finished = Some(true),
                Err(e) => cx.error(format!("Could not write {}: {e}", merged.display())),
            }
        }
    }

    /// Conflicts with no three-way text to merge: delete/modify and binary.
    fn structural(&mut self, ui: &mut Ui, cx: &mut Cx, path: &str, l: &Loaded) {
        let pal = cx.pal.clone();
        ui.add_space(12.0);
        ui.heading(path);
        let msg = if l.binary {
            "This file is binary or isn't UTF-8 text, so it can't be merged line by line. Choose which version to keep."
        } else if l.missing_ours {
            "This file was deleted on the current branch and changed on the other. Choose what to do."
        } else {
            "This file was changed on the current branch and deleted on the other. Choose what to do."
        };
        ui.label(RichText::new(msg).color(pal.dim));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if !l.missing_ours && ui.button(format!("Keep {}", self.sides.0)).clicked() {
                cx.run(ops::checkout_conflict_side(path, false));
            }
            if !l.missing_theirs && ui.button(format!("Keep {}", self.sides.1)).clicked() {
                cx.run(ops::checkout_conflict_side(path, true));
            }
            if (l.missing_ours || l.missing_theirs) && ui.button("Delete the file").clicked() {
                cx.run(ops::remove_conflicted(path));
            }
        });
    }

    fn conflict_indices(doc: &MergeDoc) -> Vec<usize> {
        doc.regions.iter().enumerate().filter(|(_, r)| matches!(r, Region::Conflict { .. })).map(|(i, _)| i).collect()
    }

    fn toolbar(&mut self, ui: &mut Ui, cx: &mut Cx, doc: &mut MergeDoc) {
        let pal = cx.pal.clone();
        let unresolved = doc.unresolved_count();
        let total = doc.conflict_count();
        // Wraps so Save stays reachable in a narrow panel.
        ui.horizontal_wrapped(|ui| {
            if unresolved == 0 {
                chip(ui, &format!("✔ All {total} conflict(s) resolved"), pal.good);
            } else {
                chip(ui, &format!("{unresolved} of {total} conflict(s) left"), pal.warn);
            }
            let indices = Self::conflict_indices(doc);
            if ui.small_button("▲").on_hover_text("Previous conflict").clicked() && !indices.is_empty() {
                self.current = (self.current + indices.len() - 1) % indices.len();
                self.scroll_to = Some(indices[self.current]);
            }
            if ui.small_button("▼").on_hover_text("Next conflict").clicked() && !indices.is_empty() {
                self.current = (self.current + 1) % indices.len();
                self.scroll_to = Some(indices[self.current]);
            }
            ui.separator();
            if ui.small_button(format!("All {}", self.sides.0)).on_hover_text(format!("Resolve remaining conflicts with {}", self.sides.0)).clicked() {
                doc.resolve_all(Resolution::Ours);
            }
            if ui.small_button(format!("All {}", self.sides.1)).on_hover_text(format!("Resolve remaining conflicts with {}", self.sides.1)).clicked() {
                doc.resolve_all(Resolution::Theirs);
            }
            if self.ai.poll() {
                for (i, lines) in self.ai.value.take().unwrap_or_default() {
                    if let Some(Region::Conflict { resolution, .. }) = doc.regions.get_mut(i) {
                        *resolution = Resolution::Custom(lines);
                    }
                }
                if let Some(e) = self.ai.error.take() {
                    cx.error(e);
                }
            }
            if ui.add_enabled(!self.ai.busy() && unresolved > 0, egui::Button::new("Resolve with AI").small()).on_hover_text("Proposes text for each open conflict. Nothing is saved until you review and save it.").clicked() {
                let cfg = cx.settings.ai.clone();
                let path = self.title();
                let open: Vec<(usize, String, String, String)> = doc.regions.iter().enumerate().filter_map(|(i, r)| match r {
                    Region::Conflict { base, ours, theirs, resolution: Resolution::Unresolved } => Some((i, base.join("\n"), ours.join("\n"), theirs.join("\n"))),
                    _ => None,
                }).collect();
                self.ai.spawn(ui.ctx(), move || -> Result<Vec<(usize, Vec<String>)>, String> {
                    let key = gitem_core::cred::ai_key(cfg.provider.key_name());
                    let mut out = Vec::new();
                    for (i, b, o, t) in open {
                        let (s, u) = gitem_core::ai::conflict_prompt(&path, &b, &o, &t, &cfg.custom_instructions);
                        let reply = gitem_core::ai::complete(&cfg, key.as_deref(), &s, &u).map_err(|e| e.to_string())?;
                        // Left open for you when the model isn't confident.
                        if reply.trim() != gitem_core::ai::UNSURE {
                            out.push((i, util::split_lines(&gitem_core::ai::clean_message(&reply))));
                        }
                    }
                    Ok(out)
                });
            }
            if self.ai.busy() {
                ui.add(egui::Spinner::new().size(13.0));
            }
            if ui.small_button("Whitespace").on_hover_text("Auto-resolve conflicts that differ only in whitespace").clicked() {
                let n = doc.resolve_whitespace_only();
                cx.toast(format!("Resolved {n} whitespace-only conflict(s)"));
            }
            toggle(ui, &mut self.show_base, "Base", "Show the common ancestor");
            if ui.selectable_label(self.raw_mode, "Edit text").on_hover_text("Edit the merged result as plain text").clicked() {
                self.raw_mode = !self.raw_mode;
                if self.raw_mode {
                    self.raw_text = doc.render("ours", "theirs");
                }
            }
            ui.separator();
            let done = if self.raw_mode { !has_conflict_markers(&self.raw_text) } else { unresolved == 0 };
            let label = if matches!(self.source, MergeSource::Repo(_)) { "Save and mark resolved" } else { "Save and finish" };
            let b = egui::Button::new(RichText::new(label).color(if done { pal.on_accent } else { pal.faint })).fill(if done { pal.accent } else { pal.panel2 });
            if ui.add_enabled(done, b).on_disabled_hover_text("Resolve every conflict first").clicked() {
                self.save(cx, doc, true);
            }
            if ui.small_button("Save with markers").on_hover_text("Write the file with conflict markers for what's left, and keep it unresolved").clicked() {
                self.save(cx, doc, false);
            }
            if matches!(self.source, MergeSource::Files { .. }) && ui.small_button("Cancel merge").clicked() {
                self.finished = Some(false);
            }
            if self.saved {
                ui.label(RichText::new("Saved").small().color(pal.good));
            }
        });
    }

    fn save(&mut self, cx: &mut Cx, doc: &MergeDoc, mark: bool) {
        let text = if self.raw_mode { self.raw_text.clone() } else { doc.render("ours", "theirs") };
        let target = match &self.source {
            MergeSource::Repo(p) => cx.repo.root.join(p),
            MergeSource::Files { merged, .. } => merged.clone(),
        };
        if let Err(e) = std::fs::write(&target, text) {
            cx.error(format!("Could not write {}: {e}", target.display()));
            return;
        }
        self.saved = true;
        match &self.source {
            MergeSource::Repo(p) => {
                if mark {
                    cx.run(ops::mark_resolved(p));
                }
            }
            MergeSource::Files { .. } => {
                if mark {
                    self.finished = Some(true);
                }
            }
        }
    }

    /// Lay out one row of panes with synced vertical scrolling across regions.
    fn panes(&mut self, ui: &mut Ui, cx: &mut Cx, doc: &mut MergeDoc) {
        let pal = cx.pal.clone();
        let font = cx.settings.editor_font_size;
        let row_h = (font * 1.45).ceil();
        let header_h = 26.0;
        let n_panes = if self.show_base { 4 } else { 3 };

        let total_w = ui.available_width();
        let (hdr, _) = ui.allocate_exact_size(vec2(total_w, 20.0), Sense::hover());
        let pane_w = (total_w - 8.0) / n_panes as f32;
        let (left, right) = if self.sides.0 == "ours" { ("OURS (current)", "THEIRS (incoming)") } else { ("UPSTREAM (rebasing onto)", "YOUR COMMIT (being rebased)") };
        let names: Vec<&str> = if self.show_base { vec![left, "BASE", "RESULT", right] } else { vec![left, "RESULT", right] };
        for (i, n) in names.iter().enumerate() {
            paint_text(ui, Rect::from_min_size(pos2(hdr.left() + i as f32 * pane_w + 6.0, hdr.top()), vec2(pane_w, 20.0)), n, prop(11.0), pal.dim);
        }

        // Region heights so we can virtualise and jump.
        let heights: Vec<f32> = doc.regions.iter().enumerate().map(|(i, r)| self.region_height(i, r, row_h, header_h)).collect();
        let mut sa = egui::ScrollArea::both().id_salt("merge-panes").auto_shrink([false, false]);
        if let Some(t) = self.scroll_to.take() {
            sa = sa.vertical_scroll_offset(heights[..t.min(heights.len())].iter().sum::<f32>());
        }
        let total_h: f32 = heights.iter().sum();
        sa.show_viewport(ui, |ui, vp| {
            ui.set_min_size(vec2(total_w - 8.0, total_h));
            let origin = ui.min_rect().min;
            let mut y = 0.0;
            for i in 0..doc.regions.len() {
                let h = heights[i];
                if y + h >= vp.top() && y <= vp.bottom() {
                    let rect = Rect::from_min_size(pos2(origin.x, origin.y + y), vec2(pane_w * n_panes as f32, h));
                    self.region_ui(ui, cx, doc, i, rect, pane_w, row_h, header_h, font, &pal);
                }
                y += h;
            }
        });
    }

    fn collapsed(&self, i: usize, r: &Region) -> bool {
        matches!(r, Region::Stable(l) if l.len() > COLLAPSE_CONTEXT * 2 + 1) && !self.expanded.contains(&i)
    }

    fn region_height(&self, i: usize, r: &Region, row_h: f32, header_h: f32) -> f32 {
        match r {
            Region::Stable(l) => {
                if self.collapsed(i, r) {
                    row_h * (COLLAPSE_CONTEXT * 2 + 1) as f32
                } else {
                    row_h * l.len() as f32
                }
            }
            Region::Auto { lines, base, .. } | Region::Same { lines, base } => row_h * lines.len().max(base.len()).max(1) as f32,
            Region::Conflict { base, ours, theirs, resolution } => {
                let result = resolved_lines(base, ours, theirs, resolution).len();
                let mut n = ours.len().max(theirs.len()).max(result).max(1);
                if self.show_base {
                    n = n.max(base.len());
                }
                if let Some((e, t)) = &self.editing
                    && *e == i {
                        n = n.max(t.lines().count() + 2);
                    }
                header_h + row_h * n as f32 + 4.0
            }
        }
    }

    fn pane_rect(&self, rect: Rect, pane_w: f32, idx: usize) -> Rect {
        Rect::from_min_size(pos2(rect.left() + idx as f32 * pane_w, rect.top()), vec2(pane_w - 2.0, rect.height()))
    }

    #[allow(clippy::too_many_arguments)]
    fn region_ui(&mut self, ui: &mut Ui, cx: &mut Cx, doc: &mut MergeDoc, i: usize, rect: Rect, pane_w: f32, row_h: f32, header_h: f32, font: f32, pal: &Palette) {
        let (o_idx, b_idx, r_idx, t_idx) = if self.show_base { (0, Some(1), 2, 3) } else { (0, None, 1, 2) };
        let collapsed = self.collapsed(i, &doc.regions[i]);
        match &mut doc.regions[i] {
            Region::Stable(lines) => {
                let shown: Vec<(Option<usize>, &str)> = if collapsed {
                    let n = lines.len();
                    let mut v: Vec<(Option<usize>, &str)> = lines[..COLLAPSE_CONTEXT].iter().enumerate().map(|(k, l)| (Some(k), l.as_str())).collect();
                    v.push((None, ""));
                    v.extend(lines[n - COLLAPSE_CONTEXT..].iter().enumerate().map(|(k, l)| (Some(k), l.as_str())));
                    v
                } else {
                    lines.iter().enumerate().map(|(k, l)| (Some(k), l.as_str())).collect()
                };
                let hidden = lines.len().saturating_sub(COLLAPSE_CONTEXT * 2);
                for (row, (marker, text)) in shown.iter().enumerate() {
                    let y = rect.top() + row as f32 * row_h;
                    if marker.is_none() {
                        let r = Rect::from_min_size(pos2(rect.left(), y), vec2(rect.width(), row_h));
                        let resp = ui.interact(r, ui.id().with(("expand", i)), Sense::click());
                        ui.painter().rect_filled(r, 0.0, if resp.hovered() { pal.hover } else { pal.panel2 });
                        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, format!("⋯ {hidden} unchanged lines (click to show)"), prop(11.0), pal.dim);
                        if resp.clicked() {
                            self.expanded.push(i);
                        }
                        continue;
                    }
                    for idx in 0..(if self.show_base { 4 } else { 3 }) {
                        let pr = self.pane_rect(rect, pane_w, idx);
                        paint_text(ui, Rect::from_min_size(pos2(pr.left() + 6.0, y), vec2(pr.width() - 8.0, row_h)), text, mono(font), pal.dim);
                    }
                }
            }
            Region::Auto { lines, base, from_theirs } => {
                let source_idx = if *from_theirs { t_idx } else { o_idx };
                let other_idx = if *from_theirs { o_idx } else { t_idx };
                let fill = if *from_theirs { pal.series[6].gamma_multiply(0.14) } else { pal.series[0].gamma_multiply(0.14) };
                for idx in [source_idx, r_idx] {
                    let pr = self.pane_rect(rect, pane_w, idx);
                    ui.painter().rect_filled(pr, 0.0, fill);
                    lines_at(ui, pr, lines, row_h, font, pal.text);
                }
                lines_at(ui, self.pane_rect(rect, pane_w, other_idx), base, row_h, font, pal.dim);
                if let Some(bi) = b_idx {
                    lines_at(ui, self.pane_rect(rect, pane_w, bi), base, row_h, font, pal.dim);
                }
            }
            Region::Same { lines, base } => {
                for idx in [o_idx, r_idx, t_idx] {
                    let pr = self.pane_rect(rect, pane_w, idx);
                    ui.painter().rect_filled(pr, 0.0, pal.good.gamma_multiply(0.1));
                    lines_at(ui, pr, lines, row_h, font, pal.text);
                }
                if let Some(bi) = b_idx {
                    lines_at(ui, self.pane_rect(rect, pane_w, bi), base, row_h, font, pal.dim);
                }
            }
            Region::Conflict { base, ours, theirs, resolution } => {
                let unresolved = *resolution == Resolution::Unresolved;
                ui.painter().rect_stroke(rect.shrink(0.5), 2.0, Stroke::new(1.0, if unresolved { pal.warn } else { pal.good.gamma_multiply(0.7) }), egui::StrokeKind::Inside);
                let body = Rect::from_min_max(pos2(rect.left(), rect.top() + header_h), rect.max);
                let fill = if unresolved { pal.conflict_bg } else { Color32::TRANSPARENT };
                let (op, tp) = (self.pane_rect(body, pane_w, o_idx), self.pane_rect(body, pane_w, t_idx));
                ui.painter().rect_filled(op, 0.0, fill);
                ui.painter().rect_filled(tp, 0.0, fill);
                lines_at(ui, op, ours, row_h, font, pal.text);
                lines_at(ui, tp, theirs, row_h, font, pal.text);
                if let Some(bi) = b_idx {
                    lines_at(ui, self.pane_rect(body, pane_w, bi), base, row_h, font, pal.dim);
                }

                // Toolbar for this conflict.
                let hdr = Rect::from_min_size(rect.min + vec2(4.0, 2.0), vec2(rect.width() - 8.0, header_h - 4.0));
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(hdr).layout(egui::Layout::left_to_right(egui::Align::Center)));
                let c = &mut child;
                c.style_mut().spacing.button_padding = vec2(5.0, 1.0);
                let mut set = |res: Resolution| *resolution = res;
                let (a, b) = self.sides;
                if c.small_button(format!("Take {a}")).clicked() {
                    set(Resolution::Ours);
                }
                if c.small_button(format!("Take {b}")).clicked() {
                    set(Resolution::Theirs);
                }
                let cap = |s: &str| -> String { s.chars().take(1).flat_map(char::to_uppercase).chain(s.chars().skip(1)).collect() };
                if c.small_button(format!("{} + {b}", cap(a))).clicked() {
                    set(Resolution::OursThenTheirs);
                }
                if c.small_button(format!("{} + {a}", cap(b))).clicked() {
                    set(Resolution::TheirsThenOurs);
                }
                if c.small_button("Base").clicked() {
                    set(Resolution::Base);
                }
                if c.small_button("Edit").clicked() {
                    let cur = resolved_lines(base, ours, theirs, resolution).join("\n");
                    self.editing = Some((i, cur));
                }
                if !unresolved && c.small_button("Undo").clicked() {
                    *resolution = Resolution::Unresolved;
                    self.editing = None;
                }

                // Result pane.
                let rp = self.pane_rect(body, pane_w, r_idx);
                let editing_here = self.editing.as_ref().is_some_and(|(e, _)| *e == i);
                if editing_here {
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rp.shrink(2.0)).layout(egui::Layout::top_down(egui::Align::Min)));
                    let mut done = false;
                    if let Some((_, text)) = &mut self.editing {
                        let te = crate::widgets::text_area(text).font(mono(font)).desired_width(rp.width() - 8.0).desired_rows(1).frame(egui::Frame::NONE);
                        child.add(te);
                        child.horizontal(|ui| {
                            done = ui.small_button("Apply").clicked();
                        });
                    }
                    if done
                        && let Some((_, text)) = self.editing.take() {
                            *resolution = Resolution::Custom(util::split_lines(&text));
                        }
                } else if unresolved {
                    ui.painter().text(rp.center(), egui::Align2::CENTER_CENTER, "Choose a resolution above", prop(12.0), pal.warn);
                } else {
                    ui.painter().rect_filled(rp, 0.0, pal.good.gamma_multiply(0.08));
                    lines_at(ui, rp, &resolved_lines(base, ours, theirs, resolution), row_h, font, pal.text);
                }
                let _ = cx;
            }
        }
    }
}

fn lines_at(ui: &Ui, pane: Rect, lines: &[String], row_h: f32, font: f32, color: Color32) {
    for (k, l) in lines.iter().enumerate() {
        let y = pane.top() + k as f32 * row_h;
        paint_text(ui, Rect::from_min_size(pos2(pane.left() + 6.0, y), vec2(pane.width() - 8.0, row_h)), l, mono(font), color);
    }
}

fn resolved_lines(base: &[String], ours: &[String], theirs: &[String], res: &Resolution) -> Vec<String> {
    match res {
        Resolution::Unresolved => Vec::new(),
        Resolution::Ours => ours.to_vec(),
        Resolution::Theirs => theirs.to_vec(),
        Resolution::OursThenTheirs => [ours, theirs].concat(),
        Resolution::TheirsThenOurs => [theirs, ours].concat(),
        Resolution::Base => base.to_vec(),
        Resolution::Custom(c) => c.clone(),
    }
}
