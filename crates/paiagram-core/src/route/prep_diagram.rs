use std::fmt::Write;
use std::iter;

use csv::Writer;
use ecow::{EcoVec, eco_vec};
use paiagram_rw::ExportObject;

use crate::route::{DiagramCache, StationRecord};
use crate::trip::{EstimateEntry, TEstimate};
use crate::{TripKey, WorldSnapshot};

#[derive(Clone, Copy)]
pub enum RouteTimetableEntry {
    NotInOperation,
    Active((TEstimate, EstimateEntry)),
}

impl std::fmt::Display for RouteTimetableEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInOperation => write!(f, ".."),
            Self::Active((estimate, _)) => write!(f, "{}", estimate.arr),
        }
    }
}

bitflags::bitflags! {
    #[derive(Clone, Copy, PartialEq)]
    pub struct StationDisplayMode: u8 {
        const ARRIVAL   = 1 << 0;
        const TRACK     = 1 << 1;
        const DEPARTURE = 1 << 2;
    }
}

/// The timetable to be displayed
#[derive(Default, Clone)]
pub struct RouteTimetable {
    /// A list of stations to display
    pub station_records: EcoVec<(StationRecord, StationDisplayMode)>,
    /// Inbound trips, i.e. downward trips. This is a sorted EcoVec
    /// where each element is sorted by first [`RouteTimetableEntry::Active`] index
    /// and time
    pub inbound_trips: EcoVec<(TripKey, EcoVec<RouteTimetableEntry>)>,
    /// Outbound trips, i.e. upward trips (but outbound trips is displayed downwards).
    /// Each element is sorted by last [`RouteTimetableEntry::Active`] index
    /// and time
    pub outbound_trips: EcoVec<(TripKey, EcoVec<RouteTimetableEntry>)>,
}

fn merge_candidates(
    mut old: EcoVec<RouteTimetableEntry>,
    range: std::ops::Range<usize>,
    new_items: impl Iterator<Item = RouteTimetableEntry> + Clone,
) -> Option<EcoVec<RouteTimetableEntry>> {
    old.extend(iter::repeat_n(
        RouteTimetableEntry::NotInOperation,
        range.end.saturating_sub(old.len()),
    ));
    for (old_ref, new_item) in iter::zip(
        old.make_mut().iter_mut().skip(range.start).take(range.clone().count()),
        new_items.clone(),
    ) {
        if !matches!(old_ref, RouteTimetableEntry::NotInOperation) {
            return None;
        }
        *old_ref = new_item;
    }
    Some(old)
}

impl RouteTimetable {
    pub fn new<'a>(
        diagram_cache: DiagramCache,
        station_records: impl Iterator<Item = &'a StationRecord>,
    ) -> Self {
        let station_records: EcoVec<_> =
            station_records.map(|record| (record.clone(), StationDisplayMode::DEPARTURE)).collect();
        let (inbound_trips, outbound_trips) = diagram_cache.map.into_iter().fold(
            (
                EcoVec::<(TripKey, EcoVec<RouteTimetableEntry>)>::new(),
                EcoVec::<(TripKey, EcoVec<RouteTimetableEntry>)>::new(),
            ),
            |(mut inbound_trips, mut outbound_trips), (trip_key, segments)| {
                for inbound_segment in segments
                    .iter()
                    .flat_map(|segment| segment.chunk_by(|prev, curr| prev.2 < curr.2))
                    .filter(|segment| segment.len() >= 2)
                {
                    let &(.., start_idx, _) = inbound_segment.first().unwrap();
                    let inbound_entries = inbound_segment
                        .iter()
                        .map(|&(es, entry, ..)| RouteTimetableEntry::Active((es, entry)));

                    if let Some((prev_key, prev_segment)) = inbound_trips.make_mut().last_mut()
                        && *prev_key == trip_key
                        && let Some(merged) = merge_candidates(
                            prev_segment.clone(),
                            (start_idx as usize)..(start_idx as usize + inbound_segment.len()),
                            inbound_entries.clone(),
                        )
                    {
                        *prev_segment = merged;
                    } else {
                        let mut col =
                            eco_vec![RouteTimetableEntry::NotInOperation; start_idx as usize];

                        col.extend(inbound_entries);
                        inbound_trips.push((trip_key, col));
                    }
                }
                (inbound_trips, outbound_trips)
            },
        );
        Self {
            station_records,
            inbound_trips,
            outbound_trips,
        }
    }

    pub fn to_csv_string(
        &self,
        snap: &WorldSnapshot,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let mut writer = Writer::from_writer(vec![]);

        // header
        writer.write_field("Trips")?;
        for s in self
            .station_records
            .iter()
            .map(|(record, _)| {
                record.station_key(&snap.graph.nodes).and_then(|key| snap.stations.get(key))
            })
            .map(|wfc| wfc.map_or_default(|wfc| wfc.name.as_str()))
        {
            writer.write_field(s)?;
        }
        writer.write_record(None::<&[u8]>)?;

        // trips
        let mut entry_fmt_buf = String::with_capacity(24);
        for (trip_key, times) in &self.inbound_trips {
            let trip_name = snap.trips.get(trip_key).map_or_default(|wfc| wfc.name.as_str());
            writer.write_field(trip_name)?;
            let mut written = 0;
            for ((_, stn_display), entry) in std::iter::zip(&self.station_records, times) {
                written += 1;
                entry_fmt_buf.clear();
                if stn_display.contains(StationDisplayMode::ARRIVAL) {
                    match entry {
                        RouteTimetableEntry::NotInOperation => write!(entry_fmt_buf, "arr:..")?,
                        RouteTimetableEntry::Active((estimate, _)) => {
                            write!(entry_fmt_buf, "arr:{}", estimate.arr)?
                        }
                    }
                }
                if !entry_fmt_buf.is_empty() {
                    write!(entry_fmt_buf, "|")?;
                }
                if stn_display.contains(StationDisplayMode::DEPARTURE) {
                    match entry {
                        RouteTimetableEntry::NotInOperation => write!(entry_fmt_buf, "dep:..")?,
                        RouteTimetableEntry::Active((estimate, _)) => {
                            write!(entry_fmt_buf, "dep:{}", estimate.dep)?
                        }
                    }
                }
                writer.write_field(entry_fmt_buf.as_str())?;
            }
            // A trip can end before the last station, leaving the row shorter than the header.
            // `csv::Writer` rejects records whose length differs from the first one, so pad it.
            for _ in written..self.station_records.len() {
                writer.write_field("")?;
            }
            writer.write_record(None::<&[u8]>)?;
        }

        // convert
        let data = String::from_utf8(writer.into_inner()?)?;
        Ok(data)
    }
}

pub struct RouteTimetableExport(pub RouteTimetable, pub WorldSnapshot);

impl ExportObject for RouteTimetableExport {
    fn extension(&self) -> impl AsRef<str> {
        ".csv"
    }
    fn write_content<W: std::io::Write>(&mut self, writer: &mut W) -> std::io::Result<()> {
        let csv =
            self.0.to_csv_string(&self.1).map_err(|e| std::io::Error::other(e.to_string()))?;
        writer.write_all(csv.as_bytes())
    }
}
