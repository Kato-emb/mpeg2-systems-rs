# Agent and contributor rules

Conventions that no lint, compiler check, or CI job can detect. What those
catch is left to them.

# Public API

The library performs no I/O: a caller pushes bytes in and pulls events out,
or hands in values and gets bytes back into a buffer it owns.

* Do not put an I/O type in the public API: no `Read`, `Write`, `AsyncRead`, file or socket. Input is `&[u8]` of any length, cut anywhere; output is written into a caller's `&mut Vec<u8>`.
* Do not make handing input to a demuxer fallible: every byte, including one skipped while unsynchronized, is reported in an event. A muxer's writing is fallible: it refuses, with `Err`, a value the specification does not allow.
* Name the driving methods `handle_input`, `poll_event` and `finish`, on both demuxers, so a reader loop written for one works for the other.
* A type that models a syntax element borrows the input it was read from. The only public types that own a copy of input are the ones a demuxer reassembled across input chunks, and those are lent to the caller from the demuxer's buffer, not cloned.
* A loop in a syntax element (program loop, elementary stream loop, descriptor loop) is a borrowed view with an `iter()` that yields `Result` per entry, so a broken entry does not hide the entries after it. Writing takes the entries as an iterator; nothing requires a `Vec`.
* Derive `Debug`, `Clone`, `PartialEq` and `Eq` on every public type; `Hash` where the fields allow it; `Copy` on a newtype or a small field-only struct. Implement `Default` only where the specification defines the default value.
* A field whose value space the specification leaves open or reserves (`stream_type`, `descriptor_tag`, `stream_id`, `table_id`) is a newtype over the integer with the known values as associated constants, never an enum: an enum gains a variant, and so breaks a `match`, every time the specification is revised.
* A newtype over an integer converts with `From` when every value is valid and with `TryFrom` when some are not (`Pid` is 13 bits), and reads back with `get`. Do not add a `new` beside them.
* A value type whose construction can fail only by a value out of its range (`Pid`, `Timestamp`) returns an error struct of its own that carries the value, as `TryFromIntError` does; parsing a syntax element, and a cursor such as `FieldReader`, report through one `Error` enum per module, named `Error` and qualified by its path. Every error type implements `core::error::Error`; its `Display` text starts lower-case, names the field and the value found, and ends without a period, so it reads well when a caller's error wraps it. A `Display` text describes its own level only and never repeats the message of the error `source` returns, so a reporter that prints the chain prints each cause once.
* A typed table or descriptor is obtained from the raw section or descriptor by an explicit conversion (`TryFrom`); the demuxer carries no type parameter and no registry, and an extension crate provides the same conversion for its own types.
* Every entry point a user drives, such as a demuxer, a muxer, a `parse` or a typed conversion, has a doctest. The crate doc has a quickstart that reads a fixture end to end.
* The summary line of a public type's doc says what the thing is to someone who does not have the specification, with the specification's term in backticks, and the next line cites the clause as Naming prescribes: ``/// Program clock reference, the 27 MHz time a decoder locks its clock to`` then ``/// `program_clock_reference_base` and `_extension`, ISO/IEC 13818-1 §2.4.3.5``.
* Where the specification leaves a choice to the user of this crate, such as which PIDs to subscribe to or how to recover wall-clock time from a PCR, the doc of the type involved says in one sentence what the crate does not do, so the user does not look for it.
* A method the specification does not name is named for what it does, following the Rust API Guidelines: `as_`/`to_`/`into_` by cost and ownership (C-CONV), getters without `get_` (C-GETTER), `iter`/`iter_mut`/`into_iter` (C-ITER).

# Rust coding guidelines

* Do not trade clarity for speed, except in code that runs once per Transport Packet or per PES Packet, such as the framer and the demux and mux loops: there, performance may win — reuse a buffer rather than allocate one per packet. Record why the obvious version was not taken in a `//` comment, and put the measurements in the PR description.
* The per-packet path allocates nothing: a reassembly buffer belongs to its PID and is cleared and reused, never dropped and recreated.
* Hand a payload to the caller as a borrow; copy it only to carry it across input chunks.
* A `perf` change carries its before-and-after numbers in the PR description, measured with the benchmarks in `benches/`.
* Do not write what `core`, `alloc`, or a crate already in the dependency graph provides, unless asked.
* Do not add features, abstractions, or configurability nobody asked for.
* Do not add a dependency to a library crate without stating in the PR why; take it with `default-features = false`, and only if it builds without std.
* Do not abbreviate variable names. Do not write `count` as `cnt`. Naming lists the exceptions.
* Relax a lint at the narrowest site that needs it, and call it out in the pull request. Never copy the workspace lint table into a crate to change one lint.
* Do not create a new file to add functionality; add it to an existing one. A new logical component, or a file past the size cap below, may take a new module.
* Do not grow a module past 500 LoC, excluding tests. Do not extend a file past roughly 800 LoC; add the functionality in a new module instead, unless the PR description states a strong reason.
* When extracting or rewriting code, do not leave its tests, its module and type docs, or its Why-not comments behind: move them with it, so the invariants stay next to the code that owns them.
* Do not create small helper methods that are referenced only once. Split code along logical component boundaries, not to shrink line counts.

# Untrusted input

* Do not size an allocation, or bound a loop, by a value read from the input without a limit the caller can set — by count and by bytes. The PES reassembly buffer of a PID is the case in point.
* Where the syntax bounds a value, bound the buffer by the syntax instead: a section is at most 4096 bytes, so section reassembly takes no setting. Only what the syntax leaves unbounded, such as a video PES packet in a Transport Stream, takes one.
* A continuity error, a packet with `transport_error_indicator` set, or a new `payload_unit_start_indicator` ends the PES packet being reassembled on that PID: report what was gathered in an event and start over; never stitch across the gap.
* Read what the specification forbids but conforming readers accept (FFmpeg, TSDuck), and keep the value as found; construct and write only what the specification allows.
* Do not write what this crate's own reader would refuse.
* Do not drop a descriptor, section or PES payload the crate does not interpret: keep its bytes, so a round trip writes it back and an extension crate can read it.

# State machines

* Do not scatter state assignment, buffer consumption and event emission over the branches. First write a pure function that classifies the input into an enum whose variants name what was observed; then apply the result in one `match` that yields the next state, the number of bytes consumed and the event to emit. An asymmetry in consumption (one branch consuming nothing) appears as data in that `match`, not as control flow.
* Do not let the classifying function decide the next state, the consumption or the event. When it returns a plan instead of an observation, the table "state × observation → transition" disappears from the code, and each branch's meaning has to be reconstructed from the pair of consumption and event.
* Do not return a bool meaning "made progress" or "keep going" from a function that drives a state machine; return an enum that names the meaning, such as `enum Progress { Advanced, NeedMoreData }`. A bool is returned only by a predicate (`is_`, `has_`).
* Do not encode three outcomes such as accept, reject and need-more-data as `Option<bool>`, with `None` standing for one of them. As with the bool, return an enum that names them.

```rust
/// What the framer saw at the head of its buffer
enum Observation {
    SyncByteAt(usize),
    NoSyncByte,
    NeedMoreData,
}

enum Progress {
    Advanced,
    NeedMoreData,
}

fn observe(buffer: &[u8], packet_size: usize) -> Observation {
    // ... pure: looks, never consumes
}

fn step(&mut self) -> Progress {
    match (self.state, observe(&self.buffer, self.packet_size)) {
        (_, Observation::NeedMoreData) => Progress::NeedMoreData,
        (State::Searching, Observation::SyncByteAt(offset)) => {
            self.skip(offset);
            self.state = State::Locked;
            Progress::Advanced
        }
        (State::Locked, Observation::NoSyncByte) => {
            self.emit(Event::Error(Error::SyncLost));
            self.state = State::Searching;
            Progress::Advanced
        }
        // ... one arm per (state, observation) pair
    }
}
```

# Tests

* Fix a bug by first adding the test that reproduces it.
* A fuzz finding becomes a unit test that holds the minimized input; the fuzz artifact itself is not committed.
* Do not rely on round trips alone: a symmetric mistake in reading and writing cancels out. Check reading against streams FFmpeg or TSDuck wrote, and writing against what they read.
* Do not generate a fixture with this crate's writer. A fixture comes from an independent implementation or from an example in the specification, and its bytes are committed.
* A golden snapshot pins only what no independent implementation can judge: the sequence of events across a sync loss, a CRC mismatch or a truncated input. Table contents and PES packet counts are checked against TSDuck and ffprobe instead.
* Do not compare fields one by one in a test; compare whole objects.
* Do not add tests for values that are statically defined.
* When logic is removed, delete its tests; do not add tests asserting the removed behavior is absent.
* Do not write the mechanics of exercising the code into a test's name or body: state the behavior, so the test reads as an executable specification.

# Comments

Each kind of information lives in one place: How in code, What in tests and doc
comments, Why in the PR description (it becomes the commit body), Why not in
`//` comments.

* Do not write `//` comments that summarize or organize what the code does — the code already says How. A `//` comment records only Why not: why an obvious-looking alternative was rejected, why a non-obvious workaround is required, or which pitfall is being avoided.
* Write a Why-not only when the alternative looks plainly better than the code and no lint, test, or compiler error would stop someone from switching to it.
* Do not put measurements, counts, or sizes in a Why-not comment: a change next to it makes them false, so they go in the PR description. A bound or ordering that correctness depends on may be stated as a relation.
* Where a branch's correctness or a loop's termination depends on a numeric relation — a minimum buffer length, a section no shorter than its header — state that relation in a Why-not comment at the code that depends on it.
* Do not put Why not in doc comments (`///` `//!`). They state the public contract — what a caller may rely on.
* Do not write a doc comment's summary line as more than one sentence, and do not end it with punctuation.
* A doctest body sits inside a doc comment, so the ban on `//` comments that summarize How does not reach it: mark the sections of a visible doctest body with brief one-line `// ...` comments, as the standard library does, so a reader can scan the example.
* `// TODO(...)` and `// FIXME(...)` are exempt from the Why-not rule; CI states what goes in the parentheses.

```rust
/// Returns the foo with the given ID
///
/// # Errors
/// * `NotFound`: the foo with the given ID was not found
pub fn foo(&self, id: FooId) -> Result<Foo> {
    // Why not bar_mut: this path must stay read-only so callers can share &self
    self.bar(id)
}
```

# Naming

Public names take their vocabulary from the specification that defines the
structure (ISO/IEC 13818-1, and ARIB STD-B10 or STD-B24 in the ARIB crate), so
that users who know the specification can navigate the API without
translation.

* In doc comments and `//` comments too, use the specification's terms verbatim (`Transport Packet`, `PES packet`, `section`, `pack`), never a synonym.
* Types take the specification's syntax element names in CamelCase, suffix included: `TransportPacket` for `transport_packet`, `TsProgramMapSection` for `TS_program_map_section`, `CaSection` for `CA_section`, `AvcVideoDescriptor` for `AVC_video_descriptor` (clippy holds the casing).
* A type carries a `#[doc(alias)]` for every other name it goes by, so rustdoc search finds it by any of them: its abbreviation (`PMT`), the name as the specification spells it when the casing differs (`TS_program_map_section`), and a name another specification gives it.
* Fields, and the accessors that return them, take the specification's field names in snake_case, verbatim, suffix included: `transport_error_indicator`, `tier_flag`, `PID` becomes `pid`. Do not shorten `tier_flag` to `tier`: which way the bool reads is then only in the specification.
* A module hosting one structure is named by its abbreviation when the specification has one (`psi::pat`, `psi::pmt`, `ps::psm`); otherwise by the syntax element name in snake_case, minus the kind the parent module already names (`descriptor::avc_video`, `ts::adaptation_field`).
* Where no specification term exists or it collides with Rust, build the closest compound from specification vocabulary (`RawDescriptor`, `SectionReassembler`) — never a synonym that renames a specification concept.
* The no-abbreviation rule does not reach the names a specification gives — field names (`ES_info_length` becomes `es_info_length`) and the abbreviations it writes for its own terms: PAT, PMT, CAT, PES, PCR, OPCR, PTS, DTS, PID, PSI, PSM, SCR, ES, STD, CRC, CA — nor `Fsm` (finite-state machine), `Demux` and `Mux`, which are accepted words in type names, locals and test names alike. An abbreviation the specifications do not write (`af` for adaptation field) is not covered: spell it out.
* The doc comment of a type that models a specification structure cites its clause: ``/// [`TsProgramMapSection`] (PMT), ISO/IEC 13818-1 §2.4.4.8``, ``/// ARIB STD-B24 Volume 3 §5.1``. The module doc does not repeat the citation; a `//` comment in the same item may cite a bare `§2.4.4.8`.

# Pull requests

* A breaking change takes `!` in the title (`feat(psi)!: ...`), whatever its type. CI cannot tell a change is breaking, and a `BREAKING CHANGE:` footer alone is not read by the release tooling.
* The title's subject is printed in the CHANGELOG as written: say what a user gains, not how it was done.

# Rules hygiene

This file is loaded in every agent session. Keep it high-signal.

* Do not describe architecture (module layout, data flow, key types): it goes stale, and the code states it.
* Do not add a rule that lint, the compiler, or CI could enforce; enforce it there instead.
* Do not edit this file during feature or fix work. Propose a rule under "Proposed additions to AGENTS.md" in the PR description; the reviewer decides.
* A new rule must be **non-obvious** (someone familiar with the code would get it wrong), **recurring** (seen more than once), and **specific enough to act on**. Editing or clarifying an existing rule is always welcome.
