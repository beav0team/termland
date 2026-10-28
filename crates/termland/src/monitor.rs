//! The live system monitor: a desktop pane built on `sysinfo` and `tui-chart`.
//!
//! Two rules decided this file, and both are worth keeping:
//!
//! * **If something already does the job, call it.** `sysinfo` samples the
//!   machine and `tui-chart` draws the graphs: braille and half-block markers,
//!   axes with automatic ticks, ring gauges, histograms, dials, legends and hit
//!   maps. None of that is written here. What *is* written here is the part no
//!   crate provides: ten seconds between a reading and a *desktop pane* — where
//!   it lives, when it samples, what the wheel does, how a click is routed.
//! * **A monitor samples at 1 Hz; the screen runs at 60.** The graph is a pure
//!   function of the samples, so the only thing between a fresh number and a
//!   redraw is who asks for the next frame.
//!
//! The one piece of glue that turned out to be necessary is [`resample`]: a 1 Hz
//! source across a 900 px pane is one sample every five pixels, and every line
//! or area widget draws isolated dots for that. Interpolating each series onto
//! the pane's own resolution is what makes the graphs read as graphs.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;
use sysinfo::{System, MINIMUM_CPU_UPDATE_INTERVAL};
use tui_chart::{
    Axis, BarChart, BarGroup, BarOrientation, Gauge, GaugeStyle, GridStyle, Histogram, HitKind,
    HitMap, LegendPosition, LineChart, LineMode, Marker, Series,
};

use crate::theme;

const CPU: Color = theme::ACCENT;
const RAM: Color = Color::Rgb(0x8f, 0xa9, 0xcf);

/// Samples kept in memory whatever the visible window: one hour at 1 Hz.
const KEEP: usize = 3600;

/// One series flattened to `(time, value)` points, as the charts want it.
type Points = Vec<(f64, f64)>;

/// Percent memory in use. The library reports *used* memory, which on Linux
/// already excludes reclaimable page cache; a `MemFree`-style number would
/// claim 90 % on an idle machine.
pub fn mem_percent(used: u64, total: u64) -> f32 {
    if total == 0 {
        return 0.0;
    }
    ((used as f64 / total as f64) * 100.0).clamp(0.0, 100.0) as f32
}

/// One instant of the two numbers this pane knows about.
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    /// Seconds since the pane was created: the x axis.
    pub t: f64,
    pub cpu: f64,
    pub ram: f64,
}

/// Which `tui-chart` widget the samples are handed to. The set is chosen for
/// legibility, not for coverage: every one of these answers a question a user
/// actually asks of a monitor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// Load over time. The default, and the only one that shows *shape*.
    Line,
    /// The same, filled — easier to read at a glance from across the room.
    Area,
    /// The window in coarse bars: what the machine was doing, bucket by bucket.
    Bars,
    /// How long the CPU has spent at each load level.
    Hist,
    /// One dial per metric, for the glanceable "is the machine busy" read.
    Dial,
}

impl View {
    pub fn name(self) -> &'static str {
        match self {
            View::Line => "line",
            View::Area => "area",
            View::Bars => "bars",
            View::Hist => "hist",
            View::Dial => "dial",
        }
    }

    pub fn all() -> &'static [View] {
        &[View::Line, View::Area, View::Bars, View::Hist, View::Dial]
    }

    /// Only these plot against time, so only these get a crosshair.
    fn over_time(self) -> bool {
        matches!(self, View::Line | View::Area)
    }
}

pub struct MonitorPane {
    pub view: View,
    /// Seconds of history on screen; the wheel changes it.
    pub window: f64,
    /// How often to sample.
    pub every: Duration,
    samples: VecDeque<Sample>,
    started: Instant,
    since: Instant,
    sys: System,
    /// Series hidden by clicking their legend entry.
    hidden: Vec<usize>,
    /// Legend rectangles from the last draw, so a click can be matched.
    legend: Vec<(usize, Rect)>,
}

impl MonitorPane {
    pub fn new() -> Self {
        let mut sys = System::new();
        // Two readings are needed before a percentage means anything; the first
        // is the baseline, so it is taken here and the pane opens with a sample.
        sys.refresh_cpu_usage();
        sys.refresh_memory();
        let mut me = Self {
            view: View::Line,
            window: 120.0,
            every: Duration::from_secs(1).max(MINIMUM_CPU_UPDATE_INTERVAL),
            samples: VecDeque::with_capacity(KEEP),
            started: Instant::now(),
            since: Instant::now() - Duration::from_secs(10),
            sys,
            hidden: Vec::new(),
            legend: Vec::new(),
        };
        me.sample();
        me
    }

    pub fn title(&self) -> String {
        format!("monitor · {}", self.view.name())
    }

    /// Sample if it is time. Returns true if a sample was taken.
    pub fn tick(&mut self) -> bool {
        if self.since.elapsed() < self.every {
            return false;
        }
        self.since = Instant::now();
        self.sample();
        true
    }

    pub fn sample(&mut self) {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        let cpu = self.sys.global_cpu_usage().clamp(0.0, 100.0) as f64;
        let ram = mem_percent(self.sys.used_memory(), self.sys.total_memory()) as f64;
        self.samples.push_back(Sample {
            t: self.started.elapsed().as_secs_f64(),
            cpu,
            ram,
        });
        while self.samples.len() > KEEP {
            self.samples.pop_front();
        }
    }

    pub fn samples(&self) -> usize {
        self.samples.len()
    }

    /// The visible window: `(t0, t1)` in seconds since start, always exactly
    /// `window` wide and always ending at the newest sample.
    ///
    /// The axis is the *window*, not the data. A monitor that re-scales its x
    /// axis to however much history exists cannot be read: the same curve means
    /// a different thing every second, and zooming in has no visible effect
    /// until the history has grown past it. So `t0` is allowed to go negative
    /// and the curve simply enters from the right, the way every system monitor
    /// since `xload` has done it.
    fn span(&self) -> (f64, f64) {
        let t1 = self.samples.back().map(|s| s.t).unwrap_or(0.0);
        (t1 - self.window, t1)
    }

    /// The visible samples, as `(x, y)` pairs per series.
    fn windowed(&self) -> (f64, f64, Points, Points) {
        let (t0, t1) = self.span();
        let mut cpu = Vec::new();
        let mut ram = Vec::new();
        for s in self.samples.iter().filter(|s| s.t >= t0) {
            cpu.push((s.t, s.cpu));
            ram.push((s.t, s.ram));
        }
        (t0, t1, cpu, ram)
    }

    fn latest(&self) -> (f64, f64) {
        self.samples
            .back()
            .map(|s| (s.cpu, s.ram))
            .unwrap_or((0.0, 0.0))
    }

    /// The x axis: relative seconds, at a precision the window can actually
    /// resolve, and never two labels that round to the same text.
    fn time_axis(t0: f64, t1: f64) -> Axis {
        let span = (t1 - t0).max(0.1);
        Axis::new()
            .bounds(t0, t1)
            .ticks(5)
            .grid_lines(GridStyle::Dotted)
            .grid_style(Style::default().fg(theme::BORDER))
            .formatter(move |v| {
                let back = (t1 - v).max(0.0);
                if back < span / 8.0 {
                    "now".to_string()
                } else if span < 30.0 {
                    format!("-{back:.1}s")
                } else if span < 600.0 {
                    format!("-{back:.0}s")
                } else {
                    format!("-{:.0}m", back / 60.0)
                }
            })
    }

    fn percent_axis() -> Axis {
        Axis::new()
            .bounds(0.0, 100.0)
            .ticks(5)
            .title("%")
            .grid_lines(GridStyle::Dotted)
            .grid_style(Style::default().fg(theme::BORDER))
            .formatter(|v| format!("{v:.0}%"))
    }

    pub fn draw(&mut self, buf: &mut Buffer, area: Rect, pointer: Option<(u16, u16)>) {
        self.legend.clear();
        if area.height < 4 || area.width < 20 {
            return;
        }
        // The last row is the pane's own status line, the same place a terminal
        // puts it, so the plot never has to share its rows with text.
        let plot = Rect {
            height: area.height - 1,
            ..area
        };
        let (t0, t1, cpu, ram) = self.windowed();
        let (cpu_now, ram_now) = self.latest();
        let hidden = self.hidden.clone();

        match self.view {
            View::Line | View::Area => {
                let area_view = self.view == View::Area;
                // Braille packs 2 dots into a cell; a filled area is one cell
                // per column, and `Block` is the only marker the crate fills
                // without stippling holes into it.
                let cols = if area_view {
                    plot.width as usize
                } else {
                    plot.width as usize * 2
                };
                let cpu = resample(&cpu, t0, t1, cols);
                let ram = resample(&ram, t0, t1, cols);
                // Area fills, so the larger series is painted first and the
                // smaller one stays visible inside it.
                let (first, second) = if area_view {
                    (("ram", ram, RAM), ("cpu", cpu, CPU))
                } else {
                    (("cpu", cpu, CPU), ("ram", ram, RAM))
                };
                let chart = LineChart::new(vec![
                    Series::new(first.0).data(first.1).color(first.2),
                    Series::new(second.0).data(second.1).color(second.2),
                ])
                .mode(if area_view {
                    LineMode::Area
                } else {
                    LineMode::Line
                })
                .marker(if area_view {
                    Marker::Block
                } else {
                    Marker::Braille
                })
                .legend(LegendPosition::TopLeft)
                .x_axis(Self::time_axis(t0, t1))
                .y_axis(Self::percent_axis())
                .hidden(hidden);
                self.capture(&chart, plot, buf);
            }
            View::Bars => {
                // Two bars for "right now" would leave a 40-row pane empty, so
                // the bars are the *window*: a bucket every six cells, each bar
                // the mean of its bucket. Same question, at a glance, for the
                // whole minute instead of the last second.
                let cols = (plot.width as usize / 6).clamp(4, 40);
                let cpu_b = bucket_means(&cpu, t0, t1, cols);
                let ram_b = bucket_means(&ram, t0, t1, cols);
                let span = t1 - t0;
                let groups: Vec<BarGroup> = (0..cols)
                    .map(|i| {
                        // One label in five, so the axis marks time without
                        // turning into a wall of numbers.
                        let label = if i % 5 == 0 {
                            let back = span * (1.0 - i as f64 / (cols - 1).max(1) as f64);
                            if span >= 600.0 {
                                format!("-{:.0}m", back / 60.0)
                            } else {
                                format!("-{back:.0}s")
                            }
                        } else {
                            String::new()
                        };
                        BarGroup::new(label, vec![cpu_b[i], ram_b[i]])
                    })
                    .collect();
                let chart = BarChart::new(groups)
                    .series_names(["cpu", "ram"])
                    .colors(vec![CPU, RAM])
                    .orientation(BarOrientation::Vertical)
                    .bar_width(1)
                    .bar_gap(2)
                    .max(100.0)
                    .value_axis(Self::percent_axis());
                self.capture(&chart, plot, buf);
            }
            View::Hist => {
                let cpu_hist = Histogram::new(cpu.iter().map(|(_, v)| *v))
                    .bins(12)
                    .range(0.0, 100.0)
                    .color(CPU)
                    .value_axis(Axis::new().ticks(4).formatter(|v| format!("{v:.0}")));
                let ram_hist = Histogram::new(ram.iter().map(|(_, v)| *v))
                    .bins(12)
                    .range(0.0, 100.0)
                    .color(RAM)
                    .value_axis(Axis::new().ticks(4).formatter(|v| format!("{v:.0}")));
                let left = Rect {
                    width: plot.width / 2,
                    ..plot
                };
                let right = Rect {
                    x: plot.x + plot.width / 2,
                    width: plot.width - plot.width / 2,
                    ..plot
                };
                self.capture(&cpu_hist, left, buf);
                self.capture(&ram_hist, right, buf);
                // A histogram has no legend and no title, and two of them side
                // by side are otherwise told apart by colour alone.
                for (rect, name, colour) in [(left, "cpu %", CPU), (right, "ram %", RAM)] {
                    buf.set_stringn(
                        rect.x + 3,
                        rect.y,
                        name,
                        rect.width as usize,
                        Style::default().fg(colour).add_modifier(Modifier::BOLD),
                    );
                }
            }
            View::Dial => {
                // Side by side, two dials of this size are clipped; stacked,
                // each one gets a box it actually fits in.
                let h = (plot.height / 2).min(14);
                let w = plot.width.min(42);
                let box_of = |row: u16| Rect {
                    x: plot.x + (plot.width - w) / 2,
                    y: plot.y + row * (plot.height / 2),
                    width: w,
                    height: h,
                };
                let dial = |v: f64, c: Color, label: &str| {
                    Gauge::new(v)
                        .style(GaugeStyle::Arc)
                        .range(0.0, 100.0)
                        .color(c)
                        .label(label.to_string())
                };
                self.capture(
                    dial(cpu_now, CPU, &format!("cpu {cpu_now:.0}%")),
                    box_of(0),
                    buf,
                );
                self.capture(
                    dial(ram_now, RAM, &format!("ram {ram_now:.0}%")),
                    box_of(1),
                    buf,
                );
            }
        }

        // The pointer reads the graph under it. Only the time charts can answer;
        // a bar, a ring or a histogram already prints its own numbers.
        if let Some((x, y)) = pointer {
            if self.view.over_time()
                && x >= plot.x
                && y >= plot.y
                && x < plot.x + plot.width
                && y < plot.y + plot.height
            {
                self.draw_readout(buf, plot, x);
            }
        }

        self.draw_status(buf, area, cpu_now, ram_now);
    }

    /// Render a widget through a `HitMap`, which is how a legend click becomes
    /// an index without duplicating the crate's legend layout here.
    fn capture(&mut self, chart: impl Widget, area: Rect, buf: &mut Buffer) {
        let map = HitMap::capture(chart, area, buf);
        for hit in map.hits() {
            if hit.kind == HitKind::Legend && !self.legend.iter().any(|(i, _)| *i == hit.series) {
                self.legend.push((hit.series, hit.rect));
            }
        }
    }

    /// The pane's status line: the hint on the left, the live numbers on the
    /// right. A monitor that only shows numbers while the mouse is over it is a
    /// monitor you have to hover to read.
    fn draw_status(&self, buf: &mut Buffer, area: Rect, cpu: f64, ram: f64) {
        let dim = Style::default().fg(theme::BORDER);
        let y = area.y + area.height - 1;
        let win = if self.window >= 60.0 {
            format!("{:.0}m", self.window / 60.0)
        } else {
            format!("{:.0}s", self.window)
        };
        let parts: [(&str, String, Color); 3] = [
            ("cpu ", format!("{cpu:.0}%"), CPU),
            ("ram ", format!("{ram:.0}%"), RAM),
            ("", win, theme::FG_DIM),
        ];
        let width: usize = parts.iter().map(|(l, v, _)| l.len() + v.len() + 2).sum();
        let hint = "1-5 view   wheel zoom   click legend";
        let room = (area.width as usize).saturating_sub(width + 3);
        if room >= 8 {
            buf.set_stringn(area.x + 1, y, hint, room, dim);
        }
        let mut x = (area.x + area.width).saturating_sub(width as u16 + 1);
        for (label, value, colour) in parts {
            if x >= area.x + area.width {
                break;
            }
            let cell = &mut buf[(x.min(area.x + area.width - 1), y)];
            let _ = cell;
            if !label.is_empty() {
                buf.set_stringn(x, y, label, area.width as usize, dim);
                x += label.len() as u16;
            }
            buf.set_stringn(
                x,
                y,
                &value,
                area.width as usize,
                Style::default().fg(colour).add_modifier(Modifier::BOLD),
            );
            x += value.len() as u16 + 2;
        }
    }

    /// The value under the pointer: a vertical rule and a caption.
    fn draw_readout(&self, buf: &mut Buffer, plot: Rect, x: u16) {
        let (t0, t1, ..) = self.windowed();
        let f = (x.saturating_sub(plot.x)) as f64 / plot.width.saturating_sub(1).max(1) as f64;
        let t = t0 + (t1 - t0) * f;
        let nearest = self
            .samples
            .iter()
            .filter(|s| s.t >= t0 && s.t <= t1)
            .min_by(|a, b| {
                (a.t - t)
                    .abs()
                    .partial_cmp(&(b.t - t).abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        let Some(s) = nearest else { return };
        for y in plot.y..plot.y + plot.height {
            if (y - plot.y).is_multiple_of(2) {
                buf.set_stringn(x, y, "┊", 1, Style::default().fg(theme::FG_DIM));
            }
        }
        let text = format!(
            "-{:.0}s  cpu {:.0}%  ram {:.0}%",
            (t1 - s.t).max(0.0),
            s.cpu,
            s.ram
        );
        let w = (text.chars().count() as u16 + 2).min(plot.width);
        let bx = plot.x + plot.width.saturating_sub(w);
        for cx in bx..bx + w {
            let cell = &mut buf[(cx, plot.y)];
            cell.set_symbol(" ");
            cell.set_bg(theme::BG_ALT);
        }
        buf.set_stringn(
            bx + 1,
            plot.y,
            &text,
            w.saturating_sub(2) as usize,
            Style::default().fg(theme::FG),
        );
    }

    pub fn legend_at(&self, x: u16, y: u16) -> Option<usize> {
        self.legend
            .iter()
            .find(|(_, r)| {
                x >= r.x
                    && y >= r.y
                    && x < r.x.saturating_add(r.width)
                    && y < r.y.saturating_add(r.height)
            })
            .map(|(i, _)| *i)
    }

    /// Keys the pane owns: the digits pick a view, +/- zoom the window.
    pub fn on_key(&mut self, ev: KeyEvent) -> bool {
        if !ev.modifiers.is_empty() {
            return false;
        }
        if let KeyCode::Char(c) = ev.code {
            if let Some(n) = c.to_digit(10) {
                let all = View::all();
                if n >= 1 && (n as usize) <= all.len() {
                    self.view = all[n as usize - 1];
                    return true;
                }
            }
            match c {
                '+' | '=' => {
                    self.zoom(1);
                    return true;
                }
                '-' => {
                    self.zoom(-1);
                    return true;
                }
                _ => {}
            }
        }
        false
    }

    /// Wheel over the graph halves or doubles the window: 15 s … 1 h.
    pub fn zoom(&mut self, steps: i32) {
        let factor = 2f64.powi(steps.clamp(-4, 4));
        self.window = (self.window * factor).clamp(15.0, 3600.0);
    }

    pub fn on_wheel(&mut self, up: bool) {
        self.zoom(if up { 1 } else { -1 });
    }

    /// A click either toggles a legend entry or is left to the desktop.
    pub fn on_click(&mut self, x: u16, y: u16) -> bool {
        let Some(i) = self.legend_at(x, y) else {
            return false;
        };
        if let Some(pos) = self.hidden.iter().position(|h| *h == i) {
            self.hidden.remove(pos);
        } else {
            self.hidden.push(i);
        }
        true
    }

    /// True while the newest sample is still being eased in, so the loop can run
    /// at the frame rate for a moment after each sample.
    pub fn animating(&self) -> bool {
        self.since.elapsed() < self.every.min(Duration::from_millis(250)) && self.samples.len() > 1
    }
}

impl Default for MonitorPane {
    fn default() -> Self {
        Self::new()
    }
}

/// Interpolate a series onto `cols` evenly spaced x values across `t0..=t1`.
///
/// A 1 Hz source in a 900 px pane is one point every five pixels, and a line
/// widget cannot join what it is not given: it draws the dots and nothing else.
/// Resampling here — once, for every time view — is what makes the graphs
/// continuous. It never invents data outside the series: before the first
/// sample there is nothing to draw, and after the last one the value is held.
pub fn resample(series: &[(f64, f64)], t0: f64, t1: f64, cols: usize) -> Vec<(f64, f64)> {
    if series.len() < 2 || cols < 2 {
        return series.to_vec();
    }
    let first = series[0].0;
    let last = series[series.len() - 1].0;
    let (a, b) = (t0.max(first), t1.max(t0.max(first)));
    if b <= a {
        return Vec::new();
    }
    let step = (b - a) / (cols - 1) as f64;
    let mut out = Vec::with_capacity(cols);
    for i in 0..cols {
        let x = a + step * i as f64;
        let j = series.partition_point(|(t, _)| *t <= x);
        let y = if j == 0 {
            series[0].1
        } else if j >= series.len() || x >= last {
            series[series.len() - 1].1
        } else {
            let ((x0, y0), (x1, y1)) = (series[j - 1], series[j]);
            if x1 <= x0 {
                y0
            } else {
                y0 + (y1 - y0) * ((x - x0) / (x1 - x0))
            }
        };
        out.push((x, y));
    }
    out
}

/// Mean of a series inside `cols` equal buckets across `t0..=t1`. Buckets with
/// no samples are left at zero: before the first reading there is no data, and
/// a bar that borrowed the next bucket's value would be inventing one.
fn bucket_means(series: &[(f64, f64)], t0: f64, t1: f64, cols: usize) -> Vec<f64> {
    let mut sums = vec![0.0f64; cols];
    let mut counts = vec![0u32; cols];
    let span = (t1 - t0).max(f64::EPSILON);
    for (t, v) in series {
        if *t < t0 || *t > t1 {
            continue;
        }
        let i = (((t - t0) / span) * cols as f64) as usize;
        let i = i.min(cols - 1);
        sums[i] += v;
        counts[i] += 1;
    }
    sums.iter()
        .zip(counts.iter())
        .map(|(s, c)| if *c == 0 { 0.0 } else { s / *c as f64 })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyModifiers;

    fn drawn(pane: &mut MonitorPane, area: Rect) -> Buffer {
        let mut b = Buffer::empty(area);
        pane.draw(&mut b, area, Some((area.x + 10, area.y + 3)));
        b
    }

    fn lit(b: &Buffer) -> usize {
        b.content.iter().filter(|c| c.symbol() != " ").count()
    }

    #[test]
    fn memory_percent_is_a_ratio_of_what_the_library_reports() {
        assert_eq!(mem_percent(0, 0), 0.0);
        assert_eq!(mem_percent(8_000_000_000, 8_000_000_000), 100.0);
        assert!((mem_percent(2_000_000_000, 8_000_000_000) - 25.0).abs() < 0.001);
        // A source reporting used > total must not publish 140 %.
        assert_eq!(mem_percent(9, 8), 100.0);
    }

    #[test]
    fn every_view_renders_something() {
        let mut m = MonitorPane::new();
        m.sample();
        m.sample();
        for view in View::all() {
            m.view = *view;
            let area = Rect::new(0, 0, 70, 20);
            let b = drawn(&mut m, area);
            assert!(lit(&b) > 40, "{view:?} drew {} cells", lit(&b));
        }
    }

    #[test]
    fn the_status_line_always_shows_the_live_numbers() {
        let mut m = MonitorPane::new();
        m.sample();
        let area = Rect::new(0, 0, 70, 20);
        let b = drawn(&mut m, area);
        let row: String = (0..area.width)
            .map(|x| b[(x, area.height - 1)].symbol().to_string())
            .collect();
        assert!(row.contains("cpu"), "status row was {row:?}");
        assert!(row.contains("ram"), "status row was {row:?}");
        assert!(row.contains('%'), "status row was {row:?}");
    }

    #[test]
    fn samples_are_percentages_and_time_only_moves_forward() {
        let mut m = MonitorPane::new();
        std::thread::sleep(MINIMUM_CPU_UPDATE_INTERVAL);
        m.sample();
        let mut last = -1.0;
        for s in &m.samples {
            assert!((0.0..=100.0).contains(&s.cpu) && (0.0..=100.0).contains(&s.ram));
            assert!(s.t >= last, "time went backwards");
            last = s.t;
        }
    }

    #[test]
    fn the_axis_is_the_window_not_the_history() {
        let mut m = MonitorPane::new();
        m.sample();
        m.window = 120.0;
        let (t0, t1) = m.span();
        assert!((t1 - t0 - 120.0).abs() < 1e-9, "span was {}", t1 - t0);
        // The right edge is the newest sample, and the axis is fixed at the
        // window from the first frame — the curve enters from the right.
        assert!((t1 - m.samples.back().unwrap().t).abs() < 1e-9);
        assert!(t0 < 0.0);
    }

    #[test]
    fn zoom_is_clamped_to_something_a_human_can_read() {
        let mut m = MonitorPane::new();
        for _ in 0..40 {
            m.zoom(-1);
        }
        assert_eq!(m.window, 15.0);
        for _ in 0..40 {
            m.zoom(1);
        }
        assert_eq!(m.window, 3600.0);
    }

    #[test]
    fn keys_switch_the_view_and_refuse_chords() {
        let mut m = MonitorPane::new();
        assert!(m.on_key(KeyEvent::new(KeyCode::Char('4'), KeyModifiers::empty())));
        assert_eq!(m.view, View::Hist);
        assert!(!m.on_key(KeyEvent::new(KeyCode::Char('4'), KeyModifiers::CONTROL)));
        assert!(!m.on_key(KeyEvent::new(KeyCode::Char('9'), KeyModifiers::empty())));
        // Every view is reachable from the keyboard, and none by an off-by-one.
        for (n, view) in View::all().iter().enumerate() {
            let key = char::from_digit(n as u32 + 1, 10).unwrap();
            assert!(m.on_key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::empty())));
            assert_eq!(m.view, *view);
        }
    }

    #[test]
    fn bucketing_averages_within_a_bucket_and_leaves_empty_ones_at_zero() {
        let series = vec![(0.0, 10.0), (0.5, 20.0), (1.0, 30.0), (3.0, 40.0)];
        let out = bucket_means(&series, 0.0, 4.0, 4);
        assert_eq!(out.len(), 4);
        assert_eq!(out[0], 15.0, "two samples in the first bucket");
        assert_eq!(out[1], 30.0);
        assert_eq!(out[2], 0.0, "an empty bucket must not borrow a neighbour");
        assert_eq!(out[3], 40.0);
    }

    #[test]
    fn clicking_a_legend_entry_hides_and_restores_a_series() {
        let mut m = MonitorPane::new();
        m.sample();
        m.sample();
        let area = Rect::new(0, 0, 70, 20);
        drawn(&mut m, area);
        let (idx, rect) = *m.legend.first().expect("the line view draws a legend");
        assert!(m.on_click(rect.x + 1, rect.y));
        assert_eq!(m.hidden, vec![idx]);
        assert!(m.on_click(rect.x + 1, rect.y));
        assert!(m.hidden.is_empty());
    }

    #[test]
    fn resampling_fills_the_gap_between_samples_without_inventing_ends() {
        let series = vec![(0.0, 10.0), (10.0, 20.0)];
        let out = resample(&series, 0.0, 10.0, 11);
        assert_eq!(out.len(), 11);
        // Interpolated, so the staircase becomes a ramp.
        assert!((out[5].1 - 15.0).abs() < 0.001, "got {:?}", out[5]);
        assert_eq!(out[0].1, 10.0);
        assert_eq!(out[10].1, 20.0);

        // Before the first sample there is nothing to draw.
        let out = resample(&series, -20.0, 10.0, 11);
        assert!(out[0].0 >= 0.0);

        // After the last one the value is held, not extrapolated.
        let out = resample(&series, 0.0, 20.0, 11);
        assert_eq!(out[10].1, 20.0);
    }

    #[test]
    fn resampling_a_flat_series_keeps_it_flat() {
        let series: Vec<(f64, f64)> = (0..5).map(|i| (i as f64, 42.0)).collect();
        let out = resample(&series, 0.0, 4.0, 9);
        assert!(out.iter().all(|(_, v)| (*v - 42.0).abs() < 1e-9));
    }
}
