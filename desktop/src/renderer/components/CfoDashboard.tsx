// Demo-only CFO dashboard for the Workbench home. All figures are dummy data
// for "Lyzr Company" — nothing here is wired to a real data source. Charts are
// hand-rolled SVG/CSS so we don't pull in a charting dependency.

const REVENUE = [2.6, 2.8, 3.0, 3.1, 3.4, 3.6, 3.9, 4.1, 4.0, 4.4, 4.7, 5.1]; // $M
const EXPENSES = [2.2, 2.3, 2.4, 2.5, 2.6, 2.7, 2.9, 3.0, 3.1, 3.2, 3.3, 3.5]; // $M
const CASH = [38, 41, 44, 46, 49, 51, 54, 56, 58, 59, 61, 62]; // $M

const KPIS = [
	{ label: "Revenue (TTM)", value: "$48.2M", delta: "+24.1%", up: true, spark: REVENUE },
	{ label: "EBITDA", value: "$11.6M", delta: "+18.4%", up: true, spark: REVENUE.map((r, i) => r - EXPENSES[i]) },
	{ label: "Cash & Equivalents", value: "$62.4M", delta: "+3.2%", up: true, spark: CASH },
	{ label: "Net Margin", value: "19.3%", delta: "-0.6pp", up: false, spark: [21, 20.5, 20, 19.8, 19.6, 19.3] },
];

const EXPENSE_MIX = [
	{ label: "R&D", value: 42, color: "var(--blue)" },
	{ label: "Sales & Mktg", value: 28, color: "var(--accent)" },
	{ label: "COGS", value: 18, color: "#5cb389" },
	{ label: "G&A", value: 12, color: "#e0a75e" },
];

const QUARTERLY_EBITDA = [
	{ label: "Q1", value: 1.8 },
	{ label: "Q2", value: 2.4 },
	{ label: "Q3", value: 2.9 },
	{ label: "Q4", value: 3.6 },
];

export function CfoDashboard() {
	return (
		<div className="cfo-dash">
			<div className="kpi-grid">
				{KPIS.map((k) => (
					<div className="kpi-card" key={k.label}>
						<div className="kpi-label">{k.label}</div>
						<div className="kpi-value">{k.value}</div>
						<div className={`kpi-delta ${k.up ? "up" : "down"}`}>
							{k.up ? "▲" : "▼"} {k.delta}
						</div>
						<Spark data={k.spark} color={k.up ? "var(--accent)" : "var(--danger)"} />
					</div>
				))}
			</div>

			<div className="dash-grid">
				<div className="dash-panel">
					<div className="dp-head">
						<span className="dp-title">Expense Mix</span>
						<span className="dp-sub">% of opex</span>
					</div>
					<div className="donut-wrap">
						<Donut segments={EXPENSE_MIX} />
						<div className="donut-legend">
							{EXPENSE_MIX.map((s) => (
								<span key={s.label}><i style={{ background: s.color }} /> {s.label} <b>{s.value}%</b></span>
							))}
						</div>
					</div>
				</div>

				<div className="dash-panel">
					<div className="dp-head">
						<span className="dp-title">EBITDA by Quarter</span>
						<span className="dp-sub">$M</span>
					</div>
					<Bars data={QUARTERLY_EBITDA} color="var(--accent)" />
				</div>
			</div>
		</div>
	);
}

function Spark({ data, color }: { data: number[]; color: string }) {
	const w = 100, h = 26;
	const max = Math.max(...data), min = Math.min(...data);
	const pts = data.map((v, i) => {
		const x = (i / (data.length - 1)) * w;
		const y = h - ((v - min) / (max - min || 1)) * (h - 3) - 1.5;
		return `${i ? "L" : "M"}${x.toFixed(1)},${y.toFixed(1)}`;
	});
	return (
		<svg className="spark" viewBox={`0 0 ${w} ${h}`} preserveAspectRatio="none">
			<path d={pts.join(" ")} fill="none" stroke={color} strokeWidth={1.5} vectorEffect="non-scaling-stroke" />
		</svg>
	);
}

function Bars({ data, color }: { data: { label: string; value: number }[]; color: string }) {
	const max = Math.max(...data.map((d) => d.value));
	return (
		<div className="bar-chart">
			{data.map((d) => (
				<div className="bar-col" key={d.label}>
					<span className="bar-val">{d.value}</span>
					<div className="bar-track">
						<div className="bar-fill" style={{ height: `${(d.value / max) * 100}%`, background: color }} />
					</div>
					<span className="bar-label">{d.label}</span>
				</div>
			))}
		</div>
	);
}

function Donut({ segments }: { segments: { label: string; value: number; color: string }[] }) {
	const total = segments.reduce((s, x) => s + x.value, 0);
	const r = 15.5, c = 2 * Math.PI * r;
	let offset = 0;
	return (
		<svg className="donut" viewBox="0 0 40 40">
			<g transform="rotate(-90 20 20)">
				<circle cx="20" cy="20" r={r} fill="none" stroke="var(--border)" strokeWidth="6" />
				{segments.map((s) => {
					const dash = (s.value / total) * c;
					const el = (
						<circle
							key={s.label}
							cx="20" cy="20" r={r} fill="none"
							stroke={s.color} strokeWidth="6"
							strokeDasharray={`${dash} ${c - dash}`}
							strokeDashoffset={-offset}
						/>
					);
					offset += dash;
					return el;
				})}
			</g>
		</svg>
	);
}
