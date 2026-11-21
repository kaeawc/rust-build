use crate::backend::{Health, TrafficRow};
use crate::{Action, State};
use rune::tok::{self, Theme, radius, type_};
use rune::{Color, Cx, El, ScrollSpec, col, el, row, text};

const GREEN: Color = Color { r: 0.20, g: 0.78, b: 0.45, a: 1.0 };
const RED: Color = Color { r: 0.92, g: 0.33, b: 0.30, a: 1.0 };
const AMBER: Color = Color { r: 0.95, g: 0.70, b: 0.25, a: 1.0 };

pub fn view(s: &State, _: &mut Cx<State>) -> El {
    let t = tok::theme(true);
    col().w_full().h_full().padding(20.0).gap(14.0).fill(t.bg).child(header(s, t)).child(cards(s, t)).child(traffic(s, t))
}

fn button(key: &str, label: &str, action: Action, primary: bool, t: Theme) -> El {
    let fill = if primary { tok::accent(0, true) } else { t.control_off };
    row()
        .key(key)
        .role("button")
        .label(label.to_string())
        .focusable(true)
        .h(28.0)
        .px(12.0)
        .center_y()
        .radius(radius::SM)
        .fill(fill)
        .on_press(move |s: &mut State| s.actions.push(action))
        .child(text(label.to_string()).font(type_::LABEL).color(t.text))
}

fn dot(c: Color) -> El {
    el().size(8.0, 8.0).radius(4.0).fill(c)
}

fn header(s: &State, t: Theme) -> El {
    let embedded = if s.embedded_running() { "Embedded: on" } else { "Embedded: off" };
    row()
        .w_full()
        .center_y()
        .gap(12.0)
        .child(text("rust-build").font(type_::TITLE).color(t.text))
        .child(text(s.url.clone()).font(type_::MONO_CAPTION).color(t.text_secondary).grow(1.0).truncate())
        .child(button("refresh-button", "Refresh", Action::Refresh, false, t))
        .child(button("embedded-button", embedded, Action::ToggleEmbedded, s.embedded_running(), t))
}

fn card(key: &str, title: &str, t: Theme) -> El {
    col()
        .key(key)
        .grow(1.0)
        .min_w(180.0)
        .padding(14.0)
        .gap(8.0)
        .radius(radius::LG)
        .fill(t.card)
        .border(1.0, t.card_border)
        .child(text(title.to_string()).font(type_::SECTION).color(t.text_secondary))
}

fn cards(s: &State, t: Theme) -> El {
    let (health_color, health_line) = match &s.health {
        Health::Unknown => (AMBER, "checking…".to_string()),
        Health::Up { latency, http_version } => (GREEN, format!("up · {http_version} · {:.1} ms", latency.as_secs_f64() * 1e3)),
        Health::Down(e) => (RED, format!("down · {e}")),
    };
    let health = card("health-card", "HEALTH", t)
        .child(row().gap(8.0).center_y().child(dot(health_color)).child(text(health_line).font(type_::BODY).color(t.text).truncate()));

    let ws_line = match (&s.ws_rtt, s.ping_pending()) {
        (_, true) => "pinging…".to_string(),
        (None, _) => "not measured".to_string(),
        (Some(Ok(d)), _) => format!("round trip {:.2} ms", d.as_secs_f64() * 1e3),
        (Some(Err(e)), _) => format!("failed · {e}"),
    };
    let ws = card("ws-card", "WEBSOCKET", t).child(text(ws_line).font(type_::BODY).color(t.text).truncate()).child(row().child(button(
        "ping-button",
        "Ping",
        Action::PingWs,
        false,
        t,
    )));

    let load_lines = match &s.load {
        None => vec!["idle".to_string()],
        Some(l) => vec![
            format!("{:.0} rps · {} errors", l.rps, l.errors),
            format!("p50 {:.2} ms · p99 {:.2} ms", l.p50.as_secs_f64() * 1e3, l.p99.as_secs_f64() * 1e3),
        ],
    };
    let mut load = card("load-card", "LOAD", t);
    for (i, line) in load_lines.into_iter().enumerate() {
        load = load.child(text(line).key(format!("load-line-{i}")).font(type_::BODY).color(t.text).truncate());
    }
    let load_button = if s.load_running() {
        button("stop-button", "Stop", Action::StopLoad, true, t)
    } else {
        button("load-button", "Run 5s load", Action::StartLoad, true, t)
    };
    load = load.child(row().child(load_button));

    row().w_full().gap(12.0).child(health).child(ws).child(load)
}

const COLS: [(&str, f32); 7] =
    [("METHOD", 64.0), ("ROUTE", 0.0), ("COUNT", 70.0), ("2XX", 60.0), ("4XX", 50.0), ("5XX", 50.0), ("AVG", 70.0)];

fn cell(content: String, i: usize, t: Theme, header: bool) -> El {
    let style = if header { type_::SECTION } else { type_::BODY };
    let color = if header { t.text_secondary } else { t.text };
    let c = text(content).font(style).color(color).truncate();
    match COLS[i].1 {
        0.0 => c.grow(1.0),
        w => c.w(w),
    }
}

fn traffic_row(r: &TrafficRow, idx: usize, t: Theme) -> El {
    let values = [
        r.method.clone(),
        r.path.clone(),
        r.count.to_string(),
        r.status2xx.to_string(),
        r.status4xx.to_string(),
        r.status5xx.to_string(),
        format!("{:.2} ms", r.avg_ms()),
    ];
    let mut line = row().key(format!("traffic-{idx}")).w_full().h(30.0).px(12.0).gap(8.0).center_y().radius(radius::SM);
    if idx.is_multiple_of(2) {
        line = line.fill(t.card);
    }
    for (i, v) in values.into_iter().enumerate() {
        line = line.child(cell(v, i, t, false));
    }
    line
}

fn traffic(s: &State, t: Theme) -> El {
    let mut head = row().key("traffic-header").w_full().h(26.0).px(12.0).gap(8.0).center_y();
    for (i, (name, _)) in COLS.iter().enumerate() {
        head = head.child(cell((*name).to_string(), i, t, true));
    }
    let mut list = col().key("traffic-list").gap(2.0);
    if s.traffic.is_empty() {
        list = list.child(text("No traffic yet").key("traffic-empty").font(type_::BODY).color(t.text_secondary).padding(12.0));
    }
    for (i, r) in s.traffic.iter().enumerate() {
        list = list.child(traffic_row(r, i, t));
    }
    col()
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .gap(4.0)
        .child(text("TRAFFIC").font(type_::SECTION).color(t.text_secondary))
        .child(head)
        .child(list.scroll(ScrollSpec { stick_bottom: false }).shrink().min_h(0.0))
}
