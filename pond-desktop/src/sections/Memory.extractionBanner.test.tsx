import { describe, it, expect, afterEach } from "vitest";
import { render, screen, cleanup } from "@testing-library/react";
import { ExtractionBanner } from "./Memory";
import type { ExtractionStatus } from "../api/types";

/** A pond reading happily: nothing wrong, nothing to say about dates. */
function healthy(over: Partial<ExtractionStatus> = {}): ExtractionStatus {
  return {
    sessions_total: 10,
    sessions_pending: 2,
    mode: "write",
    last_pass_at: new Date().toISOString(),
    last_pass_windows: 4,
    last_pass_written: 3,
    last_pass_dated: 0,
    last_pass_dates_lost: 0,
    last_pass_reminders_written: 0,
    last_pass_reminders_lost: 0,
    unattributed_sessions: 0,
    blocked_on: null,
    running: true,
    ...over,
  };
}

afterEach(() => cleanup());

describe("ExtractionBanner and the dates a pass threw away", () => {
  /// The pond this was written for. A model that puts dates in notes and files
  /// no reminders at all -- one of them did that on every one of 432 measured
  /// opportunities -- leaves `last_pass_reminders_lost` and
  /// `last_pass_reminders_written` both at 0, which is exactly what this panel
  /// used to read. It rendered nothing while every refused date was discarded.
  it("says so when a pass refused dates and the model filed no reminder", () => {
    render(
      <ExtractionBanner status={healthy({ last_pass_dated: 4, last_pass_dates_lost: 4 })} />,
    );
    expect(
      screen.getByText(/4 of the 4 date\(s\) the last pass refused were not kept anywhere/),
    ).toBeTruthy();
    expect(screen.getByText(/The model filed no reminder for them/)).toBeTruthy();
  });

  /// The same loss with the other cause, which needs a different answer: the
  /// model did its half and the store would not take the write.
  it("names the store when the write is what failed", () => {
    render(
      <ExtractionBanner
        status={healthy({
          last_pass_dated: 2,
          last_pass_dates_lost: 2,
          last_pass_reminders_lost: 2,
        })}
      />,
    );
    expect(
      screen.getByText(/The pond could not save 2 reminder\(s\) from that pass/),
    ).toBeTruthy();
  });

  /// Both counters, because the ratio is what tells a slip from a model that
  /// never files a reminder at all. `last_pass_dated` was exposed over HTTP and
  /// drawn by nothing either.
  it("shows how many of the refused dates were lost", () => {
    render(
      <ExtractionBanner
        status={healthy({
          last_pass_dated: 20,
          last_pass_dates_lost: 1,
          last_pass_reminders_written: 19,
        })}
      />,
    );
    expect(
      screen.getByText(/1 of the 20 date\(s\) the last pass refused were not kept anywhere/),
    ).toBeTruthy();
  });

  /// A failed reminder write with no dated note behind it still has no other
  /// symptom anywhere on this panel.
  it("still reports a reminder that could not be saved when no note was dated", () => {
    render(<ExtractionBanner status={healthy({ last_pass_reminders_lost: 1 })} />);
    expect(
      screen.getByText(/1 reminder\(s\) from the last pass could not be saved/),
    ).toBeTruthy();
  });

  /// The control. A clean pass must not grow a banner out of this change.
  it("says nothing about dates when none were lost", () => {
    render(<ExtractionBanner status={healthy({ last_pass_reminders_written: 3 })} />);
    expect(screen.queryByText(/were not kept anywhere/)).toBeNull();
    expect(screen.getByText(/3 date\(s\) from the last pass were kept as reminders/)).toBeTruthy();
  });
});
