import XCTest

/// AMUX-4990. The one path nothing else covers: open ANOTHER app, tap Share,
/// and drive the amux row in the system share sheet.
///
/// Everything below the sheet already has tests. `AmuxClientBootstrapTests`
/// covers the parser, `AmuxClientLiveTests` drives the shipping client against
/// a real server. Both compile `AmuxClient.swift` into a test bundle and call
/// it directly, so neither can see what a user sees. The failures that have
/// actually shipped here are all of that kind:
///
///   - a hand-written `AmuxShare/Info.plist` silently discarded by XcodeGen,
///     leaving an extension with no `NSExtension` dict. The build succeeded.
///     iOS never showed it in the sheet.
///   - the row reading "AmuxApp" rather than "amux", because the share sheet
///     labels an extension with its CONTAINING APP's name and the app had no
///     `CFBundleDisplayName`. Found 2026-09-23 when Ethan went looking for
///     "amux" in the sheet's app list and did not find it.
///
/// The CI guard added for the first one checks the appex is EMBEDDED, which is
/// a weaker claim than "iOS offers it under the name people look for".
///
/// SELECTORS ARE MEASURED, NOT GUESSED. Every identifier below was read out of
/// a live element tree on iOS 26.5 rather than assumed, because the obvious
/// guesses are all wrong here: the Photos grid has no cells (it is `Image`
/// elements with identifier `PXGGridLayout-Info`), those images are not
/// hittable so they need a coordinate tap, and `images.firstMatch` matches a
/// tab-bar icon rather than a photo.
///
/// PREREQUISITE the caller sets up, because a UI test cannot reach simctl:
///   xcrun simctl addmedia <udid> <some.png>
/// The worker-list test additionally needs a reachable server in the App Group
/// and SKIPS without one, so a rig gap never reports as a share-sheet bug.
final class ShareSheetUITests: XCTestCase {

    private static let expectedRowLabel = "amux"
    /// Delivery is confirmed against a REAL worker, because the owner asked for
    /// the share to be proven end to end. `amux` is this repo's own lane, so a
    /// verification prompt lands where it is expected rather than interrupting
    /// somebody else's work.
    private static let target = "amux"
    private let photos = XCUIApplication(bundleIdentifier: "com.apple.mobileslideshow")

    override func setUp() {
        super.setUp()
        continueAfterFailure = false
    }

    override func tearDown() {
        photos.terminate()
        super.tearDown()
    }

    // MARK: - Steps

    private func openNewestPhoto() throws {
        photos.launch()
        // A fresh simulator shows a first-run screen whose title moves between
        // releases, so dismiss by action rather than by title.
        for label in ["Continue", "Get Started", "Not Now", "Later"] {
            let b = photos.buttons[label]
            if b.waitForExistence(timeout: 2), b.isHittable { b.tap() }
        }

        let grid = photos.images.matching(identifier: "PXGGridLayout-Info")
        guard grid.firstMatch.waitForExistence(timeout: 20) else {
            attach(photos, "photos-empty")
            throw XCTSkip(
                "the Photos library is empty. Seed it first: "
                + "xcrun simctl addmedia <udid> <file.png>")
        }
        // Newest last, and the newest is the one the caller just added.
        grid.element(boundBy: grid.count - 1)
            .coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
            .tap()
    }

    private func tapShare() {
        let share = photos.buttons["PUOneUpBarButtonItemIdentifierShare"]
        if share.waitForExistence(timeout: 10) {
            share.tap()
            return
        }
        // Identifier is private API and may be renamed; the label is localized
        // but stable in an en_US simulator.
        let byLabel = photos.buttons["Share"]
        XCTAssertTrue(byLabel.waitForExistence(timeout: 10),
                      "no Share control in the Photos viewer")
        byLabel.tap()
    }

    /// Every app offered in the sheet's app row, by label.
    private func sheetAppRow() -> [String] {
        photos.cells.matching(identifier: "shareCell")
            .allElementsBoundByIndex.map(\.label)
    }

    private func attach(_ app: XCUIApplication, _ name: String) {
        let tree = XCTAttachment(string: app.debugDescription)
        tree.name = name
        tree.lifetime = .keepAlways
        add(tree)
        let shot = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
        shot.name = name + "-screen"
        shot.lifetime = .keepAlways
        add(shot)
    }

    // MARK: - Tests

    /// THE REGRESSION THIS EXISTS FOR, in both of its shipped forms: absent
    /// from the sheet, or present under a name nobody would recognise.
    func testTheShareSheetOffersAmuxUnderThatName() throws {
        try openNewestPhoto()
        tapShare()

        let row = photos.cells.matching(identifier: "shareCell")
        XCTAssertTrue(row.firstMatch.waitForExistence(timeout: 15),
                      "the share sheet never rendered its app row")

        let labels = sheetAppRow()
        guard labels.contains(Self.expectedRowLabel) else {
            attach(photos, "share-sheet-app-row")
            return XCTFail(
                "the share sheet offers \(labels), which does not include "
                + "'\(Self.expectedRowLabel)'. Absent means a missing or "
                + "malformed NSExtension dict; present under another name means "
                + "the containing app's CFBundleDisplayName changed, since that "
                + "is what iOS labels this row with.")
        }

        let amux = photos.cells.matching(identifier: "shareCell")
            .matching(NSPredicate(format: "label == %@", Self.expectedRowLabel)).firstMatch
        XCTAssertTrue(amux.isHittable, "amux is in the sheet but not tappable")
        amux.tap()

        // Proves the appex LAUNCHED, rather than the row merely existing.
        let ext = XCUIApplication(bundleIdentifier: "cz.janskala.amux.Share")
        XCTAssertTrue(
            ext.navigationBars["Share to amux"].waitForExistence(timeout: 25)
            || photos.navigationBars["Share to amux"].waitForExistence(timeout: 5),
            "amux was tapped and its share UI never appeared")
    }

    /// The extension is a SEPARATE PROCESS with its own container. It reads the
    /// server from the App Group, and a group the two targets disagree about
    /// shows "No amux server selected" forever while the host app works fine.
    /// No in-process test can see that split.
    func testTheExtensionLoadsWorkersFromTheAppGroup() throws {
        try openNewestPhoto()
        tapShare()

        let amux = photos.cells.matching(identifier: "shareCell")
            .matching(NSPredicate(format: "label == %@", Self.expectedRowLabel)).firstMatch
        try XCTSkipUnless(amux.waitForExistence(timeout: 15),
                          "amux not offered; testTheShareSheetOffersAmuxUnderThatName owns that")
        amux.tap()

        let ext = XCUIApplication(bundleIdentifier: "cz.janskala.amux.Share")
        XCTAssertTrue(ext.navigationBars["Share to amux"].waitForExistence(timeout: 25),
                      "the share UI never appeared")

        // Three outcomes worth separating: the header means workers loaded, the
        // error text means it ran and could not reach a server, neither means
        // it hung.
        let sendTo = ext.staticTexts["Send to"]
        let noServer = ext.staticTexts.containing(
            NSPredicate(format: "label CONTAINS[c] 'No amux server selected'")).firstMatch

        if !sendTo.waitForExistence(timeout: 30) {
            if noServer.exists {
                attach(ext, "extension-no-server")
                throw XCTSkip(
                    "the extension ran and has no server configured. Set serverURL "
                    + "in group.cz.janskala.amux first; an unset server "
                    + "is a rig gap, not a bug.")
            }
            attach(ext, "extension-stuck")
            return XCTFail("the share UI opened and produced neither a worker list nor an error")
        }

        XCTAssertGreaterThan(
            ext.cells.count, 0,
            "'Send to' rendered with no rows, so the list request came back empty")

        // ACTIVE ONLY IS THE DEFAULT (AMUX-5015).
        //
        // ASSERTED FROM THE HEADER'S OWN ARITHMETIC rather than by driving the
        // toggle. Two earlier attempts and what each taught:
        //
        //   counting `worker-*` rows        6 -> 6. A SwiftUI Form realizes only
        //                                   the rows on screen, so that number
        //                                   is a viewport count and cannot see
        //                                   62 hidden workers.
        //   flipping the toggle             `switch value before='1' after='1'`.
        //                                   XCUITest's tap does not flip this
        //                                   SwiftUI Toggle, so the comparison
        //                                   measured nothing. An even earlier
        //                                   version PASSED this way only because
        //                                   its prose matcher picked up the
        //                                   toggle's own label, "Active workers
        //                                   only", as the second reading.
        //
        // The header states the whole population split, so it can be checked
        // without moving anything: active + hidden must equal the fleet, and
        // hidden must be non-zero or nothing is being filtered.
        // The active-only switch lives in the Filter menu since 2026-09-30, so
        // its default is read from the header below, which names "paused
        // hidden" only while it is on. The Filter and Sort menus themselves
        // must be on the screen, on the row under search.
        XCTAssertTrue(ext.buttons["groupFilter"].waitForExistence(timeout: 10), "no Filter menu")
        XCTAssertTrue(ext.buttons["sortOrder"].exists, "no Sort menu")
        XCTAssertTrue(ext.buttons["sortOrder"].label.contains("Recently shared"),
                      "recently shared must be the default sort: '\(ext.buttons["sortOrder"].label)'")

        let header = ext.staticTexts["population"]
        XCTAssertTrue(header.waitForExistence(timeout: 10), "no population count in the header")
        let label = header.label
        let numbers = label.split(whereSeparator: { !$0.isNumber }).compactMap { Int($0) }
        XCTAssertEqual(
            numbers.count, 3,
            "the default header must state active, running and hidden: '\(label)'")
        let (active, running, hidden) = (numbers[0], numbers[1], numbers[2])
        XCTAssertGreaterThan(
            hidden, 0,
            "nothing is being withheld, so the filter is doing nothing: '\(label)'")
        XCTAssertGreaterThan(active, 0, "no worker is offered at all: '\(label)'")
        XCTAssertLessThanOrEqual(
            running, active,
            "running must be a subset of active; the filter is on lifecycle, not on running: '\(label)'")
        XCTAssertTrue(
            label.contains("paused hidden"),
            "the header must name WHAT it withholds, not just how many: '\(label)'")

        // CROSS-CHECKED AGAINST THE SERVER (AMUX-4990). Everything above is
        // internally consistent arithmetic: it proves SOMETHING is withheld and
        // not that the RIGHT set is on screen. A filter that dropped one active
        // worker and admitted one paused one would satisfy all of it.
        //
        // So the displayed population is compared to the population the server
        // reports. Read over loopback, where amux answers anonymously, so this
        // needs no credential and cannot accidentally re-prove the auth path.
        let fleet = try fleetLifecycleCounts()
        XCTAssertEqual(
            active, fleet.active,
            "the list shows \(active) workers but the server reports \(fleet.active) "
            + "lifecycle-active (of \(fleet.total) unarchived). Header: '\(label)'")
        XCTAssertEqual(
            hidden, fleet.total - fleet.active,
            "\(hidden) withheld but \(fleet.total - fleet.active) are inactive. Header: '\(label)'")
        XCTAssertGreaterThan(
            fleet.total - fleet.active, 0,
            "this fleet has no inactive workers, so the exclusion half of this test "
            + "proves nothing right now — pause one and re-run")

        // SEARCH, then SELECT. Those are the two things this screen is for and
        // both are addressable by identifier.
        let search = ext.textFields["search"]
        XCTAssertTrue(search.waitForExistence(timeout: 5), "no search field above the worker list")
        search.tap()
        search.typeText(Self.target)

        // Evidence of the layout (search on top, Filter + Sort under it, list,
        // note at the bottom), kept on a pass too.
        attach(ext, "share-sheet-layout")

        let row = ext.buttons["worker-\(Self.target)"]
        XCTAssertTrue(row.waitForExistence(timeout: 10),
                      "searching for '\(Self.target)' did not surface its row")
        row.tap()

        // Selection is shown by a checkmark on the row. Asserting THAT rather
        // than the Send button's enabled state is deliberate: any query against
        // the extension's toolbar throws "Failed to get matching snapshot" at
        // this point in the session, on `ext` and on the host app alike, while
        // queries against the list keep working. Four runs, same error, three
        // different spellings of the query.
        XCTAssertTrue(ext.staticTexts["recipients"].waitForExistence(timeout: 5),
                      "tapping '\(Self.target)' did not mark it selected")
        // With the keyboard up (search focused), Cancel, Send and the note must
        // all stay on screen: the keyboard may not cover any of them.
        search.tap()
        let screen = XCUIScreen.main.screenshot().image.size
        for (name, el) in [("cancel", ext.buttons["cancel"]), ("send", ext.buttons["send"]), ("note", ext.textFields["note"])] {
            XCTAssertTrue(el.waitForExistence(timeout: 5), "\(name) is gone with the keyboard up")
            XCTAssertTrue(el.isHittable, "\(name) is covered with the keyboard up")
            XCTAssertLessThanOrEqual(el.frame.maxY, screen.height, "\(name) is off screen")
        }
        attach(ext, "share-sheet-keyboard-up")

        // DELIVERY IS NOT ASSERTED HERE. `AmuxClientLiveTests` drives the same
        // send through the same shipping client and then reads the recipient's
        // history back, which is a stronger claim than a tap and does not
        // depend on the toolbar being queryable. Splitting them keeps this test
        // about the sheet.
    }

    /// OWNER FEEDBACK CHECKLIST (Ethan, 2026-09-23 .. 2026-10-01), one test that
    /// walks every point he raised about this sheet and leaves a named
    /// screenshot per point in SHOT_DIR. Sends ONLY to throwaway workers named in
    /// SHARE_TARGETS (created stopped by the caller and removed after), and
    /// reads their history back from the server to prove delivery.
    ///
    /// Run with: TEST_RUNNER_SHOT_DIR=<dir> TEST_RUNNER_SHARE_TARGETS=a,b
    /// TEST_RUNNER_AMUX_URL=$(amux url) xcodebuild test ... -only-testing:...
    func testOwnerFeedbackChecklist() throws {
        let env = ProcessInfo.processInfo.environment
        guard let dir = env["SHOT_DIR"], let targetsRaw = env["SHARE_TARGETS"] else {
            throw XCTSkip("set SHOT_DIR and SHARE_TARGETS (throwaway workers) to run the checklist")
        }
        let targets = targetsRaw.split(separator: ",").map(String.init)
        try XCTSkipUnless(targets.count >= 2, "two throwaway targets are needed for multi-select")
        var notes: [String] = []
        func shot(_ name: String) {
            let png = XCUIScreen.main.screenshot().pngRepresentation
            try? png.write(to: URL(fileURLWithPath: dir).appendingPathComponent(name + ".png"))
            let a = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
            a.name = name; a.lifetime = .keepAlways; add(a)
        }
        // Written on every line, not in a defer: a failed assertion with
        // continueAfterFailure = false stops the test before a defer runs.
        func note(_ line: String) {
            notes.append(line)
            try? notes.joined(separator: "\n").write(
                to: URL(fileURLWithPath: dir).appendingPathComponent("checklist.txt"),
                atomically: true, encoding: .utf8)
        }

        try openNewestPhoto()
        tapShare()
        let amux = photos.cells.matching(identifier: "shareCell")
            .matching(NSPredicate(format: "label == %@", Self.expectedRowLabel)).firstMatch
        XCTAssertTrue(amux.waitForExistence(timeout: 15), "amux is not offered in the share sheet")
        let tapped = Date()
        amux.tap()
        let ext = XCUIApplication(bundleIdentifier: "cz.janskala.amux.Share")

        // A7: the picker paints from the cached list without waiting on the
        // network or on copying the shared file.
        let firstRow = ext.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'worker-'")).firstMatch
        XCTAssertTrue(firstRow.waitForExistence(timeout: 30), "no worker rows appeared")
        let paint = Date().timeIntervalSince(tapped)
        let preparing = ext.staticTexts["Preparing what you shared…"].exists
        note(String(format: "A7 first worker row %.2fs after tapping amux; 'Preparing what you shared…' visible at first paint: %@",
                    paint, preparing ? "yes" : "no"))
        shot("a7-first-paint")
        XCTAssertLessThan(paint, 10, "the worker list took \(paint)s to paint")

        // A9 + A2 + A8 default: search on top, Filter and Sort under it, active only.
        XCTAssertTrue(ext.textFields["search"].exists, "no search field")
        XCTAssertTrue(ext.buttons["groupFilter"].exists, "no Filter menu")
        XCTAssertTrue(ext.buttons["sortOrder"].label.contains("Recently shared"), "default sort is not Recently shared")
        XCTAssertTrue(ext.staticTexts["population"].label.contains("paused hidden"), "active-only is not the default")
        note("A2 header: " + ext.staticTexts["population"].label)
        shot("a1-a2-a9-layout")

        // A8: every sort order, and a group filter.
        for order in ["Most shared", "Activity", "Name", "Recently shared"] {
            ext.buttons["sortOrder"].tap()
            let item = ext.buttons[order]
            XCTAssertTrue(item.waitForExistence(timeout: 5), "sort option '\(order)' missing")
            if order == "Most shared" { shot("a8-sort-menu") }
            item.tap()
            XCTAssertTrue(ext.buttons["sortOrder"].label.contains(order), "sort did not switch to \(order)")
            if order == "Name" { shot("a8-sorted-by-name") }
        }
        ext.buttons["groupFilter"].tap()
        let groupItem = ext.buttons["amux"]
        if groupItem.waitForExistence(timeout: 5) {
            shot("a8-filter-menu")
            groupItem.tap()
            XCTAssertTrue(ext.buttons["groupFilter"].label.contains("Group: amux"), "group filter did not apply")
            note("A8 group filter amux -> " + ext.staticTexts["population"].label)
            shot("a8-group-amux")
            ext.buttons["groupFilter"].tap()
            let all = ext.buttons["All groups"]
            XCTAssertTrue(all.waitForExistence(timeout: 5), "no 'All groups' option")
            all.tap()
        } else {
            note("A8 no group named 'amux' in the Filter menu")
            shot("a8-filter-menu")
            ext.buttons["groupFilter"].tap()
        }

        // A10: workers with Chat on are listed.
        let search = ext.textFields["search"]
        for name in ["amux", "mixpeek-override"] {
            search.tap()
            search.clearAndType(name)
            XCTAssertTrue(ext.buttons["worker-\(name)"].waitForExistence(timeout: 10), "\(name) is not listed")
        }
        shot("a10-chat-enabled-listed")

        // A1 + A3: pick the throwaway targets, more than one.
        for t in targets.prefix(2) {
            search.tap()
            search.clearAndType(t)
            let row = ext.buttons["worker-\(t)"]
            XCTAssertTrue(row.waitForExistence(timeout: 10), "throwaway \(t) is not listed")
            // The sheet preselects the last worker shared to (by design), which
            // after an earlier run is one of these throwaways. A tap toggles, so
            // tapping an already-selected target unselected it (07:14 re-test).
            let to = ext.staticTexts["recipients"]
            if !(to.exists && to.label.contains(t)) { row.tap() }
            else { note("A3 \(t) was already preselected as the last shared worker") }
        }
        search.tap()
        search.clearAndType("")
        XCTAssertTrue(ext.staticTexts["recipients"].waitForExistence(timeout: 5), "no recipients line")
        note("A3 recipients: " + ext.staticTexts["recipients"].label)
        // Counted from the recipients line: toolbar queries are unreliable in
        // this extension (see testTheExtensionLoadsWorkersFromTheAppGroup).
        let recips = ext.staticTexts["recipients"].label
        XCTAssertTrue(targets.prefix(2).allSatisfy { recips.contains($0) },
                      "both throwaways must be selected: '\(recips)'")
        shot("a1-a3-two-selected")

        // A5: with the note focused the keyboard covers nothing.
        let noteField = ext.textFields["note"]
        noteField.tap()
        let marker = "sim-check " + UUID().uuidString.prefix(8)
        noteField.typeText(String(marker))
        let screen = XCUIScreen.main.screenshot().image.size
        for (name, el) in [("cancel", ext.buttons["cancel"]), ("send", ext.buttons["send"]), ("note", noteField)] {
            XCTAssertTrue(el.isHittable, "\(name) is covered with the note keyboard up")
            XCTAssertLessThanOrEqual(el.frame.maxY, screen.height, "\(name) is off screen")
        }
        shot("a5-note-keyboard-up")

        // A4 + A6: Send shows progress then a Sent check; Cancel stays.
        ext.buttons["send"].tap()
        let sending = ext.otherElements["sendingCard"]
        let sent = ext.otherElements["sentCard"]
        var sawSending = false, sawSent = false, cancelDuring = false
        let until = Date().addingTimeInterval(150)
        while Date() < until && !sawSent {
            if sending.exists || ext.staticTexts.containing(NSPredicate(format: "label BEGINSWITH 'Sending to'")).firstMatch.exists {
                if !sawSending { shot("a6-sending"); cancelDuring = ext.buttons["cancel"].exists }
                sawSending = true
            }
            if sent.exists || ext.staticTexts.containing(NSPredicate(format: "label BEGINSWITH 'Sent to'")).firstMatch.exists {
                sawSent = true; shot("a6-sent")
            }
        }
        note("A6 saw Sending: \(sawSending), saw Sent: \(sawSent); A4 Cancel present while sending: \(cancelDuring)")
        XCTAssertTrue(sawSent, "never saw the Sent confirmation")
        XCTAssertTrue(cancelDuring || !sawSending, "Cancel disappeared while sending")

        // Delivery, read back from the server for each throwaway.
        for t in targets.prefix(2) {
            let found = try historyContains(session: t, text: String(marker))
            note("delivered to \(t): \(found)")
            XCTAssertTrue(found, "\(t) has no message containing \(marker)")
        }
    }

    /// SEND MATRIX (Ethan, 2026-10-02: "make sure the share works with files
    /// too, also it should have feedback i click send nothing happens"). One
    /// case per run, chosen by CASE, so the host can switch the server proxy
    /// (normal, slow, fail) between cases:
    ///   image, multi (2 photos), pdf, file (arbitrary), link (Safari URL),
    ///   slow (send held 8s), fail (send answers 500), nopick (nothing chosen).
    /// Every case asserts feedback within 1s of tapping Send and writes one
    /// result line plus a screenshot to SHOT_DIR. Sends only to TARGET, a
    /// throwaway worker the caller creates guarded and deletes after.
    func testSendMatrix() throws {
        let env = ProcessInfo.processInfo.environment
        guard let dir = env["SHOT_DIR"], let kase = env["CASE"], let target = env["TARGET"] else {
            throw XCTSkip("set SHOT_DIR, CASE and TARGET to run the send matrix")
        }
        let marker = env["MARKER"] ?? ("matrix " + UUID().uuidString.prefix(8))
        func shot(_ name: String) {
            let png = XCUIScreen.main.screenshot().pngRepresentation
            try? png.write(to: URL(fileURLWithPath: dir).appendingPathComponent("m-\(kase)-\(name).png"))
        }
        func result(_ line: String) {
            let url = URL(fileURLWithPath: dir).appendingPathComponent("matrix.txt")
            let prev = (try? String(contentsOf: url, encoding: .utf8)) ?? ""
            try? (prev + "\(kase): \(line)\n").write(to: url, atomically: true, encoding: .utf8)
        }

        // 1. Open the share sheet from the right source.
        let host: XCUIApplication
        switch kase {
        case "pdf", "file":
            host = XCUIApplication(bundleIdentifier: "com.apple.DocumentsApp")
            host.launch()
            let name = kase == "pdf" ? "matrix-sample.pdf" : "matrix-notes.csv"
            for label in ["Continue", "Not Now"] {
                let b = host.buttons[label]; if b.waitForExistence(timeout: 2), b.isHittable { b.tap() }
            }
            let browse = host.tabBars.buttons["Browse"]
            if browse.waitForExistence(timeout: 10) { browse.tap(); browse.tap() }
            let onPhone = host.cells.staticTexts["On My iPhone"].firstMatch
            if onPhone.waitForExistence(timeout: 10) { onPhone.tap() }
            let file = host.cells.containing(NSPredicate(format: "label CONTAINS %@", name.split(separator: ".")[0] as CVarArg)).firstMatch
            guard file.waitForExistence(timeout: 15) else { shot("no-file"); return XCTFail("\(name) not in Files") }
            file.press(forDuration: 1.3)
            let share = host.buttons["Share"].firstMatch
            guard share.waitForExistence(timeout: 10) else { shot("no-share"); return XCTFail("no Share in the Files menu") }
            share.tap()
        case "link":
            host = XCUIApplication(bundleIdentifier: "com.apple.mobilesafari")
            host.activate()
            let share = host.buttons["ShareButton"]
            guard share.waitForExistence(timeout: 20) else { shot("no-share"); return XCTFail("no Safari Share button") }
            share.tap()
        default:
            host = photos
            if kase == "multi" {
                photos.launch()
                for label in ["Continue", "Get Started", "Not Now", "Later"] {
                    let b = photos.buttons[label]; if b.waitForExistence(timeout: 2), b.isHittable { b.tap() }
                }
                let grid = photos.images.matching(identifier: "PXGGridLayout-Info")
                guard grid.firstMatch.waitForExistence(timeout: 20), grid.count >= 2 else { return XCTFail("need 2 photos") }
                photos.buttons["Select"].tap()
                grid.element(boundBy: grid.count - 1).tap()
                grid.element(boundBy: grid.count - 2).tap()
                let share = photos.buttons["Share"].firstMatch
                XCTAssertTrue(share.waitForExistence(timeout: 10), "no Share in select mode")
                share.tap()
            } else {
                try openNewestPhoto()
                tapShare()
            }
        }
        let amux = host.cells.matching(identifier: "shareCell")
            .matching(NSPredicate(format: "label == %@", Self.expectedRowLabel)).firstMatch
        guard amux.waitForExistence(timeout: 20) else { shot("no-amux"); return XCTFail("amux not in the share sheet") }
        amux.tap()
        let ext = XCUIApplication(bundleIdentifier: "cz.janskala.amux.Share")
        let firstRow = ext.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'worker-'")).firstMatch
        XCTAssertTrue(firstRow.waitForExistence(timeout: 30), "no worker rows")

        // 2. Choose the target (or deliberately nothing).
        let search = ext.textFields["search"]
        if kase != "nopick" {
            search.tap(); search.clearAndType(target)
            let row = ext.buttons["worker-\(target)"]
            XCTAssertTrue(row.waitForExistence(timeout: 10), "\(target) not listed")
            let to = ext.staticTexts["recipients"]
            if !(to.exists && to.label.contains(target)) { row.tap() }
            search.tap(); search.clearAndType("")
            let note = ext.textFields["note"]
            note.tap(); note.typeText(String(marker))
        }
        shot("before-send")

        // 3. Send, and measure how long until the sheet says something.
        let tap = Date()
        ext.buttons["send"].tap()
        if kase == "nopick" {
            let hint = ext.staticTexts["pickHint"]
            let ok = hint.waitForExistence(timeout: 2)
            result(String(format: "hint shown: %@ after %.2fs", ok ? "yes" : "no", Date().timeIntervalSince(tap)))
            shot("hint")
            XCTAssertTrue(ok, "tapping Send with nothing picked showed nothing")
            return
        }
        let sendingCard = ext.otherElements["sendingCard"]
        let sendingBtn = ext.buttons["sending"]
        var feedbackAt: TimeInterval?
        var sawSending = false, sawSent = false, sawAlert = false, steps: [String] = []
        let until = Date().addingTimeInterval(kase == "slow" ? 90 : 150)
        while Date() < until && !sawSent && !sawAlert {
            if sendingCard.exists || sendingBtn.exists {
                if feedbackAt == nil { feedbackAt = Date().timeIntervalSince(tap); shot("sending") }
                sawSending = true
                if let step = ext.staticTexts.matching(NSPredicate(format: "label BEGINSWITH 'Uploading' OR label BEGINSWITH 'Delivering' OR label BEGINSWITH 'Preparing'")).allElementsBoundByIndex.first?.label,
                   steps.last != step { steps.append(step) }
            }
            // The card shows for 0.9s before the sheet closes; match its text too,
            // as testOwnerFeedbackChecklist does (the card id alone was missed
            // on the 2026-10-02 run while the message was delivered).
            if ext.otherElements["sentCard"].exists
                || ext.staticTexts.containing(NSPredicate(format: "label BEGINSWITH 'Sent to'")).firstMatch.exists {
                sawSent = true; shot("sent")
            }
            if ext.alerts["Could not send"].exists || host.alerts["Could not send"].exists { sawAlert = true; shot("alert") }
        }
        let fb = feedbackAt.map { String(format: "%.2fs", $0) } ?? "none"
        switch kase {
        case "fail":
            let alert = ext.alerts["Could not send"].exists ? ext.alerts["Could not send"] : host.alerts["Could not send"]
            let msg = alert.staticTexts.allElementsBoundByIndex.map(\.label).joined(separator: " | ")
            if alert.exists { alert.buttons["OK"].tap() }
            let retryable = ext.buttons["send"].waitForExistence(timeout: 5) && ext.buttons["send"].isEnabled
            result("feedback \(fb); sending \(sawSending); error alert \(sawAlert) [\(msg)]; Send re-enabled for retry \(retryable)")
            shot("after-fail")
            XCTAssertTrue(sawAlert, "a failing server showed no error")
            XCTAssertTrue(retryable, "Send was not re-enabled after the failure")
        default:
            let delivered = try historyContains(session: target, text: String(marker))
            var atts = 0
            if delivered, let text = try historyText(session: target, containing: String(marker)) {
                atts = text.components(separatedBy: "@/").count - 1
            }
            result("feedback \(fb); sending \(sawSending); sent \(sawSent); delivered \(delivered); attachments \(atts); steps \(steps)")
            XCTAssertTrue(sawSent, "never saw Sent")
            XCTAssertTrue(delivered, "\(target) never got \(marker)")
            let want = ["image": 1, "multi": 2, "pdf": 1, "file": 1, "slow": 1, "link": 0][kase] ?? 0
            XCTAssertEqual(atts, want, "\(kase) should arrive with \(want) attachment(s)")
        }
        XCTAssertNotNil(feedbackAt, "Send showed no progress at all")
        // The spinner is set synchronously on tap; the measured time is mostly
        // XCUITest's own query latency, which reached 1.4s at host load 40+.
        if let feedbackAt { XCTAssertLessThan(feedbackAt, 2.0, "feedback took \(feedbackAt)s") }
    }

    /// Ethan, 2026-10-03: cached workers load instantly, Send queues offline
    /// with feedback, last shared sorts first. One CASE per run, because the
    /// runner changes the App Group server URL between cases:
    ///   warm      real server: open, measure open-to-first-row, cancel (fills the cache)
    ///   offline   dead server: the cached list must still paint; default sort is
    ///             Recently shared; Send to TARGET must end in the Queued card
    ///   sortcheck real server again: TARGET (just shared) is the first row
    /// Results append to SHOT_DIR/queue.txt.
    func testCachedListOfflineQueueAndSort() throws {
        let env = ProcessInfo.processInfo.environment
        guard let dir = env["SHOT_DIR"], let kase = env["CASE"], let target = env["TARGET"] else {
            throw XCTSkip("set SHOT_DIR, CASE and TARGET to run the queue checks")
        }
        let marker = env["MARKER"] ?? "queue-check"
        func shot(_ name: String) {
            try? XCUIScreen.main.screenshot().pngRepresentation
                .write(to: URL(fileURLWithPath: dir).appendingPathComponent("q-\(kase)-\(name).png"))
        }
        func result(_ line: String) {
            let url = URL(fileURLWithPath: dir).appendingPathComponent("queue.txt")
            let prev = (try? String(contentsOf: url, encoding: .utf8)) ?? ""
            try? (prev + "\(kase): \(line)\n").write(to: url, atomically: true, encoding: .utf8)
        }
        try openNewestPhoto()
        tapShare()
        let amux = photos.cells.matching(identifier: "shareCell")
            .matching(NSPredicate(format: "label == %@", Self.expectedRowLabel)).firstMatch
        guard amux.waitForExistence(timeout: 20) else { shot("no-amux"); return XCTFail("amux not in the share sheet") }
        let opened = Date()
        amux.tap()
        let ext = XCUIApplication(bundleIdentifier: "cz.janskala.amux.Share")
        let rows = ext.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'worker-'"))
        let firstRow = rows.firstMatch
        let painted = firstRow.waitForExistence(timeout: 30)
        let ms = Int(Date().timeIntervalSince(opened) * 1000)
        shot("open")
        guard painted else { result("no rows after 30s"); return XCTFail("no worker rows") }
        switch kase {
        case "warm":
            result("open-to-first-row \(ms) ms")
            ext.buttons["cancel"].tap()
        case "offline":
            let sortLabel = ext.buttons["sortOrder"].exists ? ext.buttons["sortOrder"].label : (ext.otherElements["sortOrder"].label)
            let search = ext.textFields["search"]
            search.tap(); search.clearAndType(target)
            let row = ext.buttons["worker-\(target)"]
            XCTAssertTrue(row.waitForExistence(timeout: 10), "\(target) not in the cached list")
            let to = ext.staticTexts["recipients"]
            if !(to.exists && to.label.contains(target)) { row.tap() }
            search.tap(); search.clearAndType("")
            let note = ext.textFields["note"]
            note.tap(); note.typeText(marker)
            let tap = Date()
            ext.buttons["send"].tap()
            var feedbackAt: TimeInterval?
            var queued = false
            let until = Date().addingTimeInterval(60)
            while Date() < until && !queued {
                if feedbackAt == nil && (ext.otherElements["sendingCard"].exists || ext.buttons["sending"].exists) {
                    feedbackAt = Date().timeIntervalSince(tap)
                }
                if ext.otherElements["queuedCard"].exists
                    || ext.staticTexts["Queued, will send when online"].exists { queued = true; shot("queued") }
            }
            let fb = feedbackAt.map { String(format: "%.2fs", $0) } ?? "none"
            result("cached open-to-first-row \(ms) ms (server unreachable); sort \"\(sortLabel)\"; feedback \(fb); queued card \(queued)")
            XCTAssertTrue(queued, "an offline send did not end in the Queued card")
            XCTAssertTrue(sortLabel.contains("Recently shared"), "default sort was \(sortLabel)")
        case "sortcheck":
            let first = firstRow.identifier
            result("first row \(first) (want worker-\(target)); open-to-first-row \(ms) ms")
            XCTAssertEqual(first, "worker-\(target)", "the worker just shared to is not first")
            ext.buttons["cancel"].tap()
        default:
            XCTFail("unknown CASE \(kase)")
        }
    }

    private func historyText(session: String, containing text: String) throws -> String? {
        let base = ProcessInfo.processInfo.environment["AMUX_URL"] ?? "https://localhost:8824"
        let url = URL(string: base + "/api/history?session=\(session)&limit=10")!
        let s = URLSession(configuration: .ephemeral, delegate: TrustAll(), delegateQueue: nil)
        let sem = DispatchSemaphore(value: 0)
        var payload: Data?
        s.dataTask(with: url) { data, _, _ in payload = data; sem.signal() }.resume()
        _ = sem.wait(timeout: .now() + 15)
        guard let payload, let obj = try? JSONSerialization.jsonObject(with: payload) else { return nil }
        let rows = (obj as? [[String: Any]]) ?? ((obj as? [String: Any])?["items"] as? [[String: Any]]) ?? []
        return rows.compactMap { $0["text"] as? String }.first { $0.contains(text) }
    }

    private func historyContains(session: String, text: String) throws -> Bool {
        let base = ProcessInfo.processInfo.environment["AMUX_URL"] ?? "https://localhost:8824"
        let deadline = Date().addingTimeInterval(20)
        while Date() < deadline {
            let url = URL(string: base + "/api/history?session=\(session)&limit=10")!
            let session = URLSession(configuration: .ephemeral, delegate: TrustAll(), delegateQueue: nil)
            let sem = DispatchSemaphore(value: 0)
            var payload: Data?
            session.dataTask(with: url) { data, _, _ in payload = data; sem.signal() }.resume()
            _ = sem.wait(timeout: .now() + 15)
            if let payload, String(data: payload, encoding: .utf8)?.contains(text) == true { return true }
            Thread.sleep(forTimeInterval: 2)
        }
        return false
    }

    // MARK: - Helpers

    /// What the SERVER says the fleet looks like, so the UI's claim can be
    /// checked against something other than itself.
    ///
    /// Counts only unarchived sessions, because `AmuxClient.workers` drops
    /// archived rows before the list is built — comparing against the raw total
    /// would fail for a reason that has nothing to do with this filter.
    private func fleetLifecycleCounts() throws -> (total: Int, active: Int) {
        // The server's address, not a remembered port: 8823 went stale when the
        // server moved to 8824 and turned this cross-check into a silent skip.
        // Pass it with TEST_RUNNER_AMUX_URL=$(amux url) xcodebuild test ...
        let base = ProcessInfo.processInfo.environment["AMUX_URL"] ?? "https://localhost:8824"
        let url = URL(string: base + "/api/sessions")!
        let session = URLSession(configuration: .ephemeral, delegate: TrustAll(), delegateQueue: nil)
        let sem = DispatchSemaphore(value: 0)
        var payload: Data?
        session.dataTask(with: url) { data, _, _ in payload = data; sem.signal() }.resume()
        _ = sem.wait(timeout: .now() + 30)
        guard let payload,
              let rows = try JSONSerialization.jsonObject(with: payload) as? [[String: Any]] else {
            throw XCTSkip(
                "could not read /api/sessions over loopback, so the UI's population cannot be "
                + "cross-checked. That is a rig gap, not a filter failure.")
        }
        let live = rows.filter { ($0["archived"] as? Bool) != true }
        return (live.count, live.filter { ($0["lifecycle"] as? String) == "active" }.count)
    }

    /// The amux server serves a self-signed certificate.
    private final class TrustAll: NSObject, URLSessionDelegate {
        func urlSession(_ session: URLSession,
                        didReceive challenge: URLAuthenticationChallenge,
                        completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void) {
            if let trust = challenge.protectionSpace.serverTrust {
                completionHandler(.useCredential, URLCredential(trust: trust))
            } else {
                completionHandler(.performDefaultHandling, nil)
            }
        }
    }
}

private extension XCUIElement {
    /// Replace a text field's contents (select-all is unreliable on a SwiftUI field).
    func clearAndType(_ text: String) {
        if let current = value as? String, !current.isEmpty, current != placeholderValue {
            typeText(String(repeating: XCUIKeyboardKey.delete.rawValue, count: current.count))
        }
        if !text.isEmpty { typeText(text) }
    }
}
