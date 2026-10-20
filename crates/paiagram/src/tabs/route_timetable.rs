use std::iter;
use std::ops::Range;

use egui::scroll_area::{ScrollAreaOutput, ScrollBarVisibility};
use egui::*;
use either::Either;
use itertools::{Itertools, chain, izip};
use paiagram_core::RouteKey;
use paiagram_core::route::{
    RouteTimetable, RouteTimetableEntry, RouteTimetableExport, StationDisplayMode,
};
use paiagram_rw::ExportObject;
use serde::{Deserialize, Serialize};

use crate::font::TIMETABLTE_TEXT_STYLE;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct RouteTimetableTab {
    /// Scroll offset shared by the row header, column header and main grid.
    scroll: Vec2,
    route_key: RouteKey,
    #[serde(skip)]
    cache: RouteTimetable,
    show_estimates: bool,
}

impl PartialEq for RouteTimetableTab {
    fn eq(&self, other: &Self) -> bool {
        self.route_key == other.route_key
    }
}

impl RouteTimetableTab {
    pub fn new(route_key: RouteKey) -> Self {
        Self {
            scroll: Vec2::ZERO,
            route_key,
            cache: RouteTimetable::default(),
            show_estimates: false,
        }
    }
}

impl super::Tab for RouteTimetableTab {
    const NAME: &'static str = "Route timetable";
    fn main_display(&mut self, app: &mut crate::App, ui: &mut Ui) {
        let Some(route) = app.routes.get(&self.route_key) else {
            ui.centered_and_justified(|ui| ui.heading("Route does not exist"));
            return;
        };

        ui.horizontal(|ui| {
            if ui.button("refresh cache").clicked() {
                self.cache = route.intervals.populate_route_timetable(&app.snap);
            }
            if ui.button("Export .csv").clicked() {
                RouteTimetableExport(self.cache.clone(), app.snap.clone())
                    .write_to_file::<false>(Default::default());
            }
            ui.checkbox(&mut self.show_estimates, "Show estimates");
        });

        let cell_size: Vec2 = vec2(50.0, 20.0);
        let total_rows: usize = self
            .cache
            .station_records
            .iter()
            .map(|record| record.1.bits().count_ones())
            .sum::<u32>() as usize;
        let total_cols: usize = self.cache.inbound_trips.len();

        // remove spacing between buttons
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        // force floating to avoid bouncing ui when scrolled to the end
        ui.style_mut().spacing.scroll.floating = true;
        // Remove button background
        ui.visuals_mut().widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;

        show_station_header(app, ui, cell_size, &mut self.cache, &mut self.scroll);
        show_trip_header(
            app,
            ui,
            cell_size,
            total_cols,
            &self.cache,
            &mut self.scroll,
        );
        show_grid(
            self.show_estimates,
            app,
            ui,
            cell_size,
            total_rows,
            total_cols,
            &self.cache,
            &mut self.scroll,
        );
    }
}

/// Left panel: the station names and their per-row display modes.
fn show_station_header(
    app: &crate::App,
    ui: &mut Ui,
    cell_size: Vec2,
    cache: &mut RouteTimetable,
    scroll: &mut Vec2,
) {
    let left = Panel::left("left panel")
        .frame(Frame::NONE)
        .drag_to_open(true)
        .default_size(cell_size.x * 2.0)
        .show(ui, |ui| {
            let cell_size = vec2(ui.available_width(), cell_size.y);
            ui.add_sized(cell_size, Label::new("Stations").truncate());
            ScrollArea::vertical()
                .auto_shrink(false)
                .vertical_scroll_offset(scroll.y)
                .scroll_bar_visibility(scroll_area::ScrollBarVisibility::AlwaysHidden)
                .show(ui, |ui| {
                    let records = cache.station_records.make_mut();
                    for (record, display_mode) in records {
                        let station_name = record
                            .station_key(app.snap.graph.nodes())
                            .and_then(|key| app.snap.stations.get(key))
                            .map_or_default(|wfc| wfc.name.as_str());
                        let mut cell_size = cell_size;
                        cell_size.y *= display_mode.bits().count_ones().at_least(1) as f32;
                        let row_type_atoms = ContainerAtom::new(Atoms::from_iter(
                            display_mode
                                .iter()
                                .map(|mode| match mode {
                                    StationDisplayMode::ARRIVAL => Atom::from("Ar."),
                                    StationDisplayMode::TRACK => Atom::from("Tr."),
                                    StationDisplayMode::DEPARTURE => Atom::from("De."),
                                    _ => Atom::from("??"),
                                })
                                .map(|atm| {
                                    atm.atom_grow(false)
                                        .atom_shrink(false)
                                        .atom_align(Align2::RIGHT_TOP)
                                }),
                        ))
                        .align2(Align2::RIGHT_CENTER)
                        .direction(Direction::TopDown);
                        let mut button = Button::new(Atoms::from_iter(iter::chain(
                            iter::once(
                                Atom::from(station_name)
                                    .atom_grow(true)
                                    .atom_align(Align2::LEFT_CENTER),
                            ),
                            row_type_atoms.into_atoms(),
                        )))
                        .truncate()
                        .min_size(cell_size)
                        .atom_ui(ui);
                        button.set_intrinsic_size(cell_size);
                        Popup::menu(&button.response)
                            .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
                            .show(|ui| {
                                let mut disp_bigflag = |flag: StationDisplayMode, atoms: &str| {
                                    let mut flag_bool = display_mode.contains(flag);
                                    // disallow removing if it is the only activated flag
                                    ui.add_enabled(
                                        !(display_mode.bits().count_ones() == 1 && flag_bool),
                                        Checkbox::new(&mut flag_bool, atoms),
                                    );
                                    display_mode.set(flag, flag_bool);
                                };
                                disp_bigflag(StationDisplayMode::ARRIVAL, "Arrival");
                                disp_bigflag(StationDisplayMode::TRACK, "Track");
                                disp_bigflag(StationDisplayMode::DEPARTURE, "Departure");
                            });
                    }
                })
        });
    scroll.y = left.inner.state.clamped_offset().y;
}

/// Top panel: trip names, following the grid's horizontal scroll.
fn show_trip_header(
    app: &crate::App,
    ui: &mut Ui,
    cell_size: Vec2,
    total_cols: usize,
    cache: &RouteTimetable,
    scroll: &mut Vec2,
) {
    let top = Panel::top("top panel").frame(Frame::NONE).show(ui, |ui| {
        show_grid_viewport(
            ScrollArea::horizontal()
                .auto_shrink([false, true])
                .scroll_bar_visibility(scroll_area::ScrollBarVisibility::AlwaysHidden)
                .horizontal_scroll_offset(scroll.x),
            ui,
            cell_size,
            1,
            total_cols,
            |ui, rect, _row_range, col_range| {
                for (col_idx, (key, _)) in cache.inbound_trips[col_range].iter().enumerate() {
                    let name = app.snap.trips.get(key).map_or_default(|wfc| wfc.name.as_str());
                    let fill = app
                        .snap
                        .trips
                        .get(key)
                        .and_then(|wfc| wfc.service_class)
                        .and_then(|class_key| app.snap.service_classes.get(&class_key))
                        .map(|wfc| wfc.style.color)
                        .unwrap_or(ui.visuals().faint_bg_color);
                    let cell_rect = Rect::from_min_size(
                        pos2(rect.left() + col_idx as f32 * cell_size.x, rect.top()),
                        cell_size,
                    );
                    ui.visuals_mut().widgets.inactive.corner_radius = 0.into();
                    ui.visuals_mut().widgets.hovered.corner_radius = 0.into();
                    ui.visuals_mut().widgets.active.corner_radius = 0.into();
                    ui.visuals_mut().widgets.inactive.weak_bg_fill = fill;
                    ui.visuals_mut().widgets.hovered.weak_bg_fill = fill.gamma_multiply(0.5);
                    ui.visuals_mut().widgets.active.weak_bg_fill = fill.gamma_multiply(0.25);
                    // ui.visuals_mut().widgets.inactive.bg_stroke;
                    ui.visuals_mut().widgets.hovered.bg_stroke = Stroke {
                        width: 1.0,
                        color: fill,
                    };
                    ui.visuals_mut().widgets.active.bg_stroke = Stroke {
                        width: 1.0,
                        color: fill.gamma_multiply(0.5),
                    };
                    ui.place(
                        cell_rect,
                        Button::new(RichText::new(name).color(if fill.intensity() > 0.5 {
                            Color32::BLACK
                        } else {
                            Color32::WHITE
                        }))
                        .truncate(),
                    );
                }
            },
        )
    });
    scroll.x = top.inner.state.clamped_offset().x;
}

/// Center panel: the timetable grid itself.
fn show_grid(
    show_estimates: bool,
    app: &crate::App,
    ui: &mut Ui,
    cell_size: Vec2,
    total_rows: usize,
    total_cols: usize,
    cache: &RouteTimetable,
    scroll: &mut Vec2,
) {
    // window stroke for drawing strokes
    let stroke = ui.visuals().window_stroke;

    let center = CentralPanel::no_frame().show(ui, |ui| {
        show_grid_viewport(
            ScrollArea::both()
                .auto_shrink(false)
                .vertical_scroll_offset(scroll.y)
                .horizontal_scroll_offset(scroll.x)
                .scroll_bar_visibility(ScrollBarVisibility::AlwaysVisible),
            ui,
            cell_size,
            total_rows,
            total_cols,
            |ui, rect, row_range, col_range| {
                for idx in cache
                    .station_records
                    .iter()
                    .flat_map(|(_, disp_mode)| disp_mode.iter())
                    .skip(row_range.start)
                    .take(row_range.clone().count())
                    .tuple_windows()
                    .enumerate()
                    .filter_map(|(idx, (prev, curr))| {
                        use StationDisplayMode as Sdm;
                        match [prev, curr] {
                            [Sdm::ARRIVAL, Sdm::ARRIVAL] => false,
                            [Sdm::ARRIVAL, Sdm::TRACK] => true,
                            [Sdm::ARRIVAL, Sdm::DEPARTURE] => true,
                            [Sdm::DEPARTURE, Sdm::ARRIVAL] => false,
                            [Sdm::DEPARTURE, Sdm::TRACK] => true,
                            [Sdm::DEPARTURE, Sdm::DEPARTURE] => false,
                            [Sdm::TRACK, _] => true,
                            _ => false,
                        }
                        .then_some(idx)
                    })
                {
                    let y = rect.top() + (idx + 1) as f32 * cell_size.y;
                    ui.painter().hline(rect.x_range(), y, stroke);
                }
                for (col_idx, (_, section)) in cache.inbound_trips[col_range].iter().enumerate() {
                    let left = rect.left() + col_idx as f32 * cell_size.x;
                    let entry_iter = izip!(
                        &cache.station_records,
                        chain!(
                            section.iter().copied().map(Either::Left),
                            iter::once(Either::Right(())),
                            iter::repeat(Either::Left(RouteTimetableEntry::NotInOperation)),
                        ),
                    )
                    .flat_map(|((_, display_mode), entry)| {
                        display_mode.iter().map(move |disp_mode| (disp_mode, entry))
                    })
                    .scan(None, |display_progress, (disp_mode, entry)| {
                        let ret = if disp_mode == StationDisplayMode::TRACK
                            && let Either::Left(RouteTimetableEntry::Active((_, entry))) = entry
                        {
                            let track_name = app
                                .snap
                                .graph
                                .nodes()
                                .get(&entry.node_key())
                                .map_or("?".into(), |wfc| wfc.name.as_str());
                            Atom::from(track_name).atom_shrink(true)
                        } else {
                            let text_to_display = match entry {
                                Either::Left(RouteTimetableEntry::Active((es, entry))) => {
                                    *display_progress = Some(true);
                                    match [show_estimates, disp_mode == StationDisplayMode::ARRIVAL]
                                    {
                                        [true, true] => es.arr.to_oud2_string(false),
                                        [true, false] => es.dep.to_oud2_string(false),
                                        [false, true] => entry.to_oud2_string_arr(false),
                                        [false, false] => entry.to_oud2_string_dep(false),
                                    }
                                }
                                Either::Right(()) if *display_progress == Some(true) => {
                                    *display_progress = Some(false);
                                    "=====".into()
                                }
                                Either::Left(RouteTimetableEntry::NotInOperation) | _ => {
                                    if *display_progress == Some(true) {
                                        "||".into()
                                    } else {
                                        "..".into()
                                    }
                                }
                            };
                            Atom::from(
                                WidgetText::Text(text_to_display)
                                    .text_style(TIMETABLTE_TEXT_STYLE.clone()),
                            )
                            .atom_align(Align2::RIGHT_CENTER)
                        };
                        Some(ret)
                    })
                    .skip(row_range.start)
                    .take(row_range.clone().count());

                    for (row_idx, atoms) in entry_iter.enumerate() {
                        let top = rect.top() + row_idx as f32 * cell_size.y;
                        let cell_rect = Rect::from_min_size(pos2(left, top), cell_size);
                        ui.place(cell_rect, WidgetAtom::new(atoms));
                    }
                    // offset by one cell
                    ui.painter().vline(left + cell_size.x, rect.y_range(), stroke);
                }
            },
        )
    });
    *scroll = center.inner.state.clamped_offset();
}

/// Like [`ScrollArea::show_viewport`], but specialized for a grid of uniformly-sized cells.
///
/// The callback only has to lay out the visible cells: it receives the screen-space rect
/// covering the visible range together with the row and column index ranges that intersect the
/// viewport.
fn show_grid_viewport<R>(
    scroll_area: ScrollArea,
    ui: &mut Ui,
    cell_size: Vec2,
    total_rows: usize,
    total_cols: usize,
    add_contents: impl FnOnce(&mut Ui, Rect, Range<usize>, Range<usize>) -> R,
) -> ScrollAreaOutput<R> {
    let row_pitch = cell_size.y;
    let col_pitch = cell_size.x;

    scroll_area.show_viewport(ui, |ui, viewport| {
        ui.set_height(row_pitch * total_rows as f32);
        ui.set_width(col_pitch * total_cols as f32);

        let row_range = visible_range(viewport.y_range(), row_pitch, total_rows);
        let col_range = visible_range(viewport.x_range(), col_pitch, total_cols);

        let origin = ui.max_rect().min;
        let rect = Rect::from_min_max(
            origin
                + vec2(
                    col_range.start as f32 * col_pitch,
                    row_range.start as f32 * row_pitch,
                ),
            origin
                + vec2(
                    col_range.end as f32 * col_pitch,
                    row_range.end as f32 * row_pitch,
                ),
        );

        ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
            add_contents(ui, rect, row_range, col_range)
        })
        .inner
    })
}

/// The indices intersecting `viewport` for evenly spaced cells of the given pitch, padded by one
/// cell on either side so that partially visible cells are included.
fn visible_range(viewport: Rangef, pitch: f32, total: usize) -> Range<usize> {
    let mut min = (viewport.min / pitch).floor().max(0.0) as usize;
    let mut max = (viewport.max / pitch).ceil() as usize + 1;
    if max > total {
        let diff = max.saturating_sub(min);
        max = total;
        min = total.saturating_sub(diff);
    }
    min..max
}
