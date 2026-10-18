use egui::scroll_area::ScrollBarVisibility;
use egui::*;
use paiagram_core::RouteKey;
use paiagram_core::route::{RouteTimetable, RouteTimetableEntry, RouteTimetableExport};
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
        });

        let cell_size: Vec2 = vec2(45.0, 18.0);
        let total_rows: usize = self.cache.station_records.len();
        let total_cols: usize = self.cache.inbound_trips.len();

        // Like `ScrollArea::show_rows`, but for the columns: only the visible columns are laid
        // out. Each cell has the same size, so we can jump straight to the visible range.
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        let col_pitch = cell_size.x;
        let row_pitch = cell_size.y;
        let grid_height = row_pitch * total_rows as f32;

        // All three scroll areas share this offset. Each starts from it, and whichever one the
        // user scrolled reports back an updated offset for the others to follow.
        let mut scroll = self.scroll;

        // window stroke for drawing strokes
        let stroke = ui.visuals().window_stroke;

        // row header
        let left = Panel::left("left panel").frame(Frame::NONE).drag_to_open(true).show(ui, |ui| {
            let max_size = [ui.available_width(), cell_size.y];
            ui.add_sized(max_size, Label::new("Stations").truncate());
            ScrollArea::vertical()
                .auto_shrink(false)
                .vertical_scroll_offset(scroll.y)
                .scroll_bar_visibility(scroll_area::ScrollBarVisibility::AlwaysHidden)
                .show(ui, |ui| {
                    ui.vertical(|ui| {
                        for i in 0..total_rows {
                            ui.add_sized(max_size, Label::new(format!("{}", i)).truncate());
                        }
                    })
                })
        });
        scroll.y = left.inner.state.offset.y;

        // Column header: follows the grid's horizontal scroll.
        let top = Panel::top("top panel").frame(Frame::NONE).show(ui, |ui| {
            ScrollArea::horizontal()
                .auto_shrink([false, true])
                .scroll_bar_visibility(scroll_area::ScrollBarVisibility::AlwaysHidden)
                .horizontal_scroll_offset(scroll.x)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for (key, _) in &self.cache.inbound_trips {
                            let name =
                                app.snap.trips.get(key).map_or_default(|wfc| wfc.name.as_str());
                            ui.add_sized(cell_size, Label::new(name));
                        }
                    })
                })
        });
        scroll.x = top.inner.state.offset.x;

        // Main grid: scrolls both ways; whichever area moved last wins for the next frame.
        ui.global_style_mut(|s| {
            s.spacing.scroll.floating = false;
        });
        let center = CentralPanel::no_frame().show(ui, |ui| {
            ScrollArea::both()
                .auto_shrink(false)
                .vertical_scroll_offset(scroll.y)
                .horizontal_scroll_offset(scroll.x)
                .scroll_bar_visibility(ScrollBarVisibility::AlwaysVisible)
                .show_viewport(ui, |ui, viewport| {
                    ui.set_height(grid_height);
                    ui.set_width(col_pitch * total_cols as f32);

                    let mut min_col = (viewport.min.x / col_pitch).floor() as usize;
                    let mut max_col = (viewport.max.x / col_pitch).ceil() as usize + 1;
                    if max_col > total_cols {
                        let diff = max_col.saturating_sub(min_col);
                        max_col = total_cols;
                        min_col = total_cols.saturating_sub(diff);
                    }

                    let x_min = ui.max_rect().left() + min_col as f32 * col_pitch;
                    let x_max = ui.max_rect().left() + max_col as f32 * col_pitch;
                    let rect = Rect::from_x_y_ranges(x_min..=x_max, ui.max_rect().y_range());

                    ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
                        ui.horizontal_top(|ui| {
                            for (_, section) in &self.cache.inbound_trips[min_col..max_col] {
                                ui.vertical(|ui| {
                                    for entry in section {
                                        let disp = match entry {
                                            RouteTimetableEntry::Skip => "||".into(),
                                            RouteTimetableEntry::NotInOperation => "..".into(),
                                            RouteTimetableEntry::Active((es, _)) => {
                                                es.dep.to_oud2_str(false)
                                            }
                                        };
                                        ui.add_sized(
                                            cell_size,
                                            Label::new(
                                                WidgetText::Text(disp)
                                                    .text_style(TIMETABLTE_TEXT_STYLE.clone()),
                                            ),
                                        );
                                    }
                                });
                            }
                        });
                        for col in min_col..max_col {
                            let x = rect.left() + (col - min_col + 1) as f32 * col_pitch;
                            ui.painter().vline(x, rect.y_range(), stroke);
                        }
                    });
                })
        });
        scroll = center.inner.state.offset;

        self.scroll = scroll;
    }
}
