use std::fmt::Write;

use csv::Writer;
use ecow::{EcoVec, eco_vec};
use paiagram_rw::ExportObject;

use crate::route::{DiagramCache, StationRecord};
use crate::trip::{EstimateEntry, TEstimate};
use crate::{TripKey, WorldSnapshot};

#[derive(Clone, Copy)]
pub enum RouteTimetableEntry {
    Skip,
    NotInOperation,
    Active((TEstimate, EstimateEntry)),
}

impl std::fmt::Display for RouteTimetableEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Skip => write!(f, "||"),
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

fn sign_of_diff(a: u32, b: u32) -> i8 {
    use std::cmp::Ordering;
    match b.cmp(&a) {
        // b is greater than a, inbound
        Ordering::Greater => 1,
        // b is smaller than a, outbound
        Ordering::Less => -1,
        // same. Can't tell if inbound or outbound
        Ordering::Equal => 0,
    }
}

impl RouteTimetable {
    pub fn new<'a>(
        diagram_cache: DiagramCache,
        station_records: impl Iterator<Item = &'a StationRecord>,
    ) -> Self {
        let (inbound_trips, outbound_trips) = diagram_cache.map.into_iter().fold(
            (EcoVec::new(), EcoVec::new()),
            |(mut inbound_trips, mut outbound_trips), (trip_key, segments)| {
                for segment in segments.iter().flat_map(|segment| {
                    let mut current_sign = 0i8;
                    segment.chunk_by(move |a, b| {
                        let new_sign = sign_of_diff(a.2, b.2);
                        if new_sign == 0 {
                            true // Treat flat points as part of the same chunk
                        } else if current_sign == 0 || current_sign == new_sign {
                            current_sign = new_sign;
                            true
                        } else {
                            current_sign = new_sign;
                            false // Break chunk on direction change
                        }
                    })
                }) {
                    // check the direction
                    let (Some(first), Some(last)) = (segment.first(), segment.last()) else {
                        continue;
                    };
                    let direction = sign_of_diff(first.2, last.2);
                    // same or inbound
                    if direction >= 0 {
                        let mut entries =
                            eco_vec![RouteTimetableEntry::NotInOperation; first.2 as usize];
                        entries.extend(
                            segment
                                .iter()
                                .map(|entry| RouteTimetableEntry::Active((entry.0, entry.1))),
                        );
                        inbound_trips.push((trip_key, entries))
                    }
                    // same or outbound
                    if direction <= 0 {
                        let mut entries =
                            eco_vec![RouteTimetableEntry::NotInOperation; last.2 as usize];
                        entries.extend(
                            segment
                                .iter()
                                .rev()
                                .map(|entry| RouteTimetableEntry::Active((entry.0, entry.1))),
                        );
                        outbound_trips.push((trip_key, entries))
                    }
                }
                (inbound_trips, outbound_trips)
            },
        );
        Self {
            station_records: station_records
                .map(|record| (record.clone(), StationDisplayMode::DEPARTURE))
                .collect(),
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
                        RouteTimetableEntry::Skip => write!(entry_fmt_buf, "arr:||")?,
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
                        RouteTimetableEntry::Skip => write!(entry_fmt_buf, "dep:||")?,
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
