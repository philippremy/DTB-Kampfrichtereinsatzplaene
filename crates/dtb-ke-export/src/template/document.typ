// Kampfrichtereinsatzplan — fixed-form document.
//
// All competition data arrives as one JSON value (`/data.json`, produced by
// `crate::model`); every judge name, label and remark is a *data value* here,
// never markup, so user input can never be interpreted as Typst syntax.
// Static German boilerplate (intro, dress code, closing note) lives in this
// file on purpose — it is part of the form, not the data.

#let data = json("/data.json")

// ── Page & type ────────────────────────────────────────────────────────────────

#set document(date: auto, author: "DTB Kampfrichtereinsatzpläne", title: data.title)
#set text(font: "Archivo", size: 11pt, lang: "de")
#set par(justify: false, leading: 0.62em)
#set list(marker: [--], indent: 0.4cm, spacing: 0.5em)

// Both logos are `place`d out of flow inside a fixed-height header box so they
// can (a) bleed past the horizontal content margin — further left / right than
// body text ever goes — and (b) be positioned independently of the title.
// Negative `dx` on the left logo and positive `dx` on the right one push them
// toward the page edges; the box's bottom overlapping the title's box is fine
// (the title rarely reaches that far across).
#let header-block = block(height: 3cm, width: 100%, {
  // The emblem's exact on-page size (cm) is computed in `crate::svg` from the
  // SVG's aspect (landscape → fit 6cm × 2.25cm, portrait → 2.25cm high) and
  // passed in, so it renders at its true size — flush-left, not centred inside
  // a fixed box. The swoosh fits inside a 4×4cm box.
  let left-logo = if data.org_logo != none {
    image(
      data.org_logo,
      width: data.org_logo_width_cm * 1cm,
      height: data.org_logo_height_cm * 1cm,
    )
  } else {
    text(size: 22pt, weight: "bold", tracking: 1pt)[DTB]
  }
  place(left + bottom, dx: -0.5cm, left-logo)
  if data.turnen_logo != none {
    place(
      right + top,
      dx: 1.5cm,
      dy: 0.5cm,
      image(data.turnen_logo, fit: "contain", width: 4cm, height: 4cm),
    )
  }
})

#set page(
  paper: "a4",
  // This is the top margin for every page *after* the first; page one pulls
  // its title up with a negative `v()` (below). `header-ascent` is large so the
  // logos sit at a fixed height regardless of the margin. The logos live in
  // the header (margin) area and are not bound by the horizontal margin.
  margin: (top: 4.5cm, bottom: 1.6cm, x: 2cm),
  header: header-block,
  header-ascent: 1.5cm,
)

// ── Small helpers ─────────────────────────────────────────────────────────────

#let styled-run(run) = {
  let b = run.text
  if run.bold { b = strong(b) }
  if run.italic { b = emph(b) }
  if run.underline { b = underline(b) }
  b
}

// One discipline's judge assignments as a tight two-column list.
// `row-gutter` is the dominant vertical cost: an STL-M list has 10 gaps, so
// every 0.1em here is ~1mm per table. 1em is the practical ceiling that
// still lets two STL-M pairs sit under a two-line page-one title.
#let judge-list(rows) = {
  set par(leading: 0.5em)
  grid(
    columns: (auto, 1fr),
    row-gutter: 1em,
    column-gutter: 6pt,
    ..rows.map(r => (text(size: 10.5pt)[#r.role:], text(size: 10.5pt)[#r.judge])).flatten(),
  )
}

// Up to two tables per row, kept together on one page (`breakable: false`) so a
// judging table never splits across a page break. Tuned so a full pair of
// STL-M tables still fits below the page-one title.
#let table-rows(tables) = {
  for pair in tables.chunks(2) {
    block(breakable: false, width: 100%, above: 0.4cm, below: 0.4cm, {
      rect(
        width: 100%,
        inset: (x: 8pt, y: 6pt),
        stroke: 0.6pt + luma(40%),
        radius: 1pt,
        grid(
          columns: (1fr, 1fr),
          column-gutter: 12pt,
          ..pair.map(t => [
            #text(weight: "bold", size: 11.5pt)[#t.label] \
            #text(style: "italic", size: 9.5pt, fill: luma(30%))[#t.discipline]
          ]),
        ),
      )
      v(0.2cm)
      grid(
        columns: (1fr, 1fr),
        column-gutter: 12pt,
        align: top,
        ..pair.map(t => judge-list(t.rows)),
      )
      v(0.5cm)
    })
  }
}

// ── Page one: title + tables ──────────────────────────────────────────────────

// Page one only: pull the title up so it sits closer to the logos than the
// body text does on every later page.
#v(-0.75cm)

#align(center, {
  // Typst folds "Archivo Condensed ExtraBold" into the "Archivo" family (it
  // strips width + weight words from name-ID 1 and ignores name-ID 16), so the
  // condensed cut is reached by variant: stretch 75% (usWidthClass 3) + 800.
  block(
    text(size: 22pt, font: "Archivo", weight: "extrabold", stretch: 75%)[#data.title],
    width: 90%,
  )
  v(0.15cm)
  text(size: 14pt, weight: "bold", style: "italic")[
    am #data.date#if data.location != "" [ in #data.location]
  ]
  v(0.28cm)
  text(size: 11pt)[
    Liebe Kampfrichter\*innen, \
    vielen Dank, dass Ihr Euch für den Einsatz zur Verfügung stellt.
    Folgende Kampfrichtereinteilung wurde für den Wettkampf vorgenommen:
  ]
})

#v(0.5cm)

// Qualification then (if any) finale tables — no phase headings; finale
// tables are marked by a "(Finale)" suffix on their discipline sub-heading
// (added in `crate::model`). Grouping is kept only so a finale table never
// shares a row with a qualification one.
#for phase in data.phases {
  table-rows(phase.tables)
}

// ── Briefing page ─────────────────────────────────────────────────────────────

#pagebreak(weak: true)

#block(
  text(weight: "bold", size: 12pt)[
    Die Kampfrichterbesprechung findet am #data.date #data.briefing.time_clause
    in Kampfrichterkleidung statt.
  ],
width: 90%)

#v(0.7cm)

#{
  if data.briefing.spare.len() == 0 {
    [Ersatzkampfrichter\*innen: --]
    linebreak()
  } else {
    for group in data.briefing.spare {
      let suffix = if group.label != none [ (#group.label)] else []
      [Ersatzkampfrichter\*innen#suffix: #group.names.join(", ")]
      linebreak()
    }
  }
}

#v(0.25cm)

Kampfrichterverantwortliche\*r: #{
  if data.briefing.responsible.len() > 0 {
    data.briefing.responsible.join(", ")
  } else { [--] }
}

#v(0.7cm)

#underline[Kampfrichterkleidung:]

#list(
  [einfarbige schwarze Hose],
  [weiße Bluse mit langem Arm / weißes Hemd mit langem Arm],
  [schwarze Schuhe (keine Stöckelschuhe)],
  [schwarzer Blazer / schwarzes Sakko],
  [langes Haar zu einer ordentlichen Frisur zusammengesteckt],
)

#v(0.7cm)

Unmittelbar nach Wettkampfende wird eine verpflichtende Nachbesprechung für alle
Kampfrichter\*innen stattfinden. Bitte berücksichtigt dies in Eurem Zeitplan.

// ── Free-form remarks ────────────────────────────────────────────────────────

#if data.remarks.len() > 0 {
  v(0.9cm)
  line(length: 100%, stroke: 0.4pt + luma(60%))
  v(0.35cm)
  for para in data.remarks {
    block(spacing: 0.7em, para.runs.map(styled-run).join())
  }
}
