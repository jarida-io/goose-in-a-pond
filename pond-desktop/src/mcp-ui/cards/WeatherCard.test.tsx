import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import "../index";
import { WeatherCard } from "./WeatherCard";
import { dayLabel } from "./weatherPayload";
import { findCardByHint, findCardRenderer } from "../registry";

afterEach(() => cleanup());

const TOOL = "giap-weather__get_weather_forecast";

function days(n: number) {
  return Array.from({ length: n }, (_, i) => ({
    date: `2026-11-${String(i + 1).padStart(2, "0")}`,
    description: i === 0 ? "Thunderstorm" : "Partly cloudy",
    temp_max_c: 24.6 + i,
    temp_min_c: 13.2,
    precipitation_sum_mm: 0,
    precipitation_probability_pct: 10 * i,
    wind_speed_max_kmh: 12,
    uv_index_max: 8,
    sunrise: "06:30",
    sunset: "18:40",
  }));
}

describe("WeatherCard registration", () => {
  it("draws both weather tools", () => {
    expect(findCardByHint("weather")?.component).toBe(WeatherCard);
    expect(findCardRenderer(TOOL)?.key).toBe("weather");
    expect(findCardRenderer("giap-weather__get_current_weather")?.key).toBe("weather");
  });
});

describe("WeatherCard forecast", () => {
  it("draws the place, the dates covered and a row per day", () => {
    render(
      <WeatherCard data={{ location: "Nairobi, Kenya", forecast: days(3) }} toolName={TOOL} />,
    );
    expect(screen.getByText("Nairobi, Kenya")).toBeTruthy();
    const rows = screen.getAllByRole("listitem");
    expect(rows).toHaveLength(3);

    const first = new Date(2026, 10, 1);
    expect(screen.getByText(dayLabel(first))).toBeTruthy();
    expect(rows[0].textContent).toContain("25°");
    expect(rows[0].textContent).toContain("13°");
    expect(rows[0].getAttribute("aria-label")).toBe(
      `${dayLabel(first)}, Thunderstorm, High 25°, Low 13°, 0% chance of rain`,
    );
    expect(rows[2].textContent).toContain("20%");
    expect(screen.queryByText("Fetching weather...")).toBeNull();
  });

  it("lists a week at most and counts the rest", () => {
    render(<WeatherCard data={{ location: "Nairobi", forecast: days(10) }} toolName={TOOL} />);
    expect(screen.getAllByRole("listitem")).toHaveLength(7);
    expect(screen.getByText("3 more days")).toBeTruthy();
  });

  it("counts one more day in the singular", () => {
    render(<WeatherCard data={{ location: "Nairobi", forecast: days(8) }} toolName={TOOL} />);
    expect(screen.getByText("1 more day")).toBeTruthy();
  });

  it("falls back to a heading when the forecast names no place", () => {
    render(<WeatherCard data={{ forecast: days(1) }} toolName={TOOL} variant="compact" />);
    expect(screen.getByText("Forecast")).toBeTruthy();
    expect(screen.getAllByRole("listitem")).toHaveLength(1);
  });
});

describe("WeatherCard current reading", () => {
  const CURRENT = {
    location: "Nairobi, Kenya",
    temperature: 21.6,
    feels_like: 20,
    condition: "Partly cloudy",
    humidity: 64,
    wind_speed: 11,
    is_day: true,
  };

  it("draws the temperature, condition, place, humidity and wind as before", () => {
    render(
      <WeatherCard data={CURRENT} toolName="giap-weather__get_current_weather" variant="normal" />,
    );
    expect(screen.getByText("22")).toBeTruthy();
    expect(screen.getByText("Partly cloudy")).toBeTruthy();
    expect(screen.getByText("Nairobi, Kenya")).toBeTruthy();
    expect(screen.getByText("64%")).toBeTruthy();
    expect(screen.getByText("11 km/h")).toBeTruthy();
    expect(screen.queryByRole("listitem")).toBeNull();
  });

  it("leaves the humidity and wind out of the compact card", () => {
    render(
      <WeatherCard data={CURRENT} toolName="giap-weather__get_current_weather" variant="compact" />,
    );
    expect(screen.getByText("22")).toBeTruthy();
    expect(screen.queryByText("64%")).toBeNull();
  });
});

describe("WeatherCard with nothing to draw", () => {
  it("shows progress only while the result has not arrived", () => {
    render(<WeatherCard data={{}} toolName={TOOL} />);
    expect(screen.getByText("Fetching weather...")).toBeTruthy();
    cleanup();
    render(<WeatherCard data={{ id: "call-1" }} toolName={TOOL} />);
    expect(screen.getByText("Fetching weather...")).toBeTruthy();
  });

  it("says so instead of spinning when a finished forecast holds no day", () => {
    render(<WeatherCard data={{ location: "Nairobi", forecast: [] }} toolName={TOOL} />);
    expect(screen.queryByText("Fetching weather...")).toBeNull();
    expect(
      screen.getByText("The weather tool answered, but sent nothing this card can show."),
    ).toBeTruthy();
  });

  it("shows the tool's own answer when it sent text and no card data", () => {
    render(
      <WeatherCard data={{ result: "The weather service did not answer." }} toolName={TOOL} />,
    );
    expect(screen.queryByText("Fetching weather...")).toBeNull();
    expect(screen.getByText("The weather service did not answer.")).toBeTruthy();
  });
});
