import { describe, it, expect, beforeAll } from "vitest";

// Trigger all card registrations
import "./cards/WeatherCard";
import "./cards/CalendarCard";
import "./cards/MapCard";
import "./cards/CryptoCard";
import "./cards/SmartHomeCard";
import "./cards/NewsCard";

import {
  getAllRegistrations,
  findCardRenderer,
  findCardByHint,
} from "./registry";

describe("MCP Card Registry", () => {
  const EXPECTED_CARDS = ["weather", "calendar", "map", "crypto", "smarthome", "news"];

  it("all 6 cards are registered", () => {
    const regs = getAllRegistrations();
    const keys = regs.map((r) => r.key);
    for (const key of EXPECTED_CARDS) {
      expect(keys).toContain(key);
    }
  });

  it("each card has required fields", () => {
    for (const key of EXPECTED_CARDS) {
      const reg = getAllRegistrations().find((r) => r.key === key);
      expect(reg).toBeDefined();
      expect(reg!.label).toBeTruthy();
      expect(reg!.icon).toBeTruthy();
      expect(reg!.toolPattern).toBeTruthy();
      expect(reg!.component).toBeTypeOf("function");
    }
  });

  it("each card has mockData", () => {
    for (const key of EXPECTED_CARDS) {
      const reg = getAllRegistrations().find((r) => r.key === key);
      expect(reg!.mockData).toBeDefined();
      expect(typeof reg!.mockData).toBe("object");
    }
  });
});

describe("Tool Pattern Matching", () => {
  it("weather tool matches weather card", () => {
    expect(findCardRenderer("giap-weather__get_current_weather")?.key).toBe("weather");
    expect(findCardRenderer("get_current_weather")?.key).toBe("weather");
  });

  it("news tool matches news card", () => {
    expect(findCardRenderer("giap-news__get_headlines")?.key).toBe("news");
    expect(findCardRenderer("giap-news__get_top_stories")?.key).toBe("news");
    expect(findCardRenderer("giap-news__search_news")?.key).toBe("news");
  });

  it("crypto/finance tool matches crypto card", () => {
    expect(findCardRenderer("giap-finance__get_crypto_price")?.key).toBe("crypto");
  });

  it("map/navigate tool matches map card", () => {
    expect(findCardRenderer("giap-maps__navigate")?.key).toBe("map");
    expect(findCardRenderer("get_directions")?.key).toBe("map");
  });

  it("home tool matches smarthome card", () => {
    expect(findCardRenderer("giap-homeassistant__get_status")?.key).toBe("smarthome");
  });

  it("calendar tool matches calendar card", () => {
    expect(findCardRenderer("giap-calendar__get_events")?.key).toBe("calendar");
  });

  it("findCardByHint returns exact match", () => {
    expect(findCardByHint("weather")?.key).toBe("weather");
    expect(findCardByHint("crypto")?.key).toBe("crypto");
    expect(findCardByHint("news")?.key).toBe("news");
    expect(findCardByHint("nonexistent")).toBeNull();
  });
});

describe("Weather Card Mock Data Shape", () => {
  it("has required weather fields", () => {
    const reg = getAllRegistrations().find((r) => r.key === "weather");
    const d = reg!.mockData!;
    expect(d.location).toBeTruthy();
    expect(typeof d.temperature).toBe("number");
    expect(typeof d.humidity).toBe("number");
    expect(typeof d.wind_speed).toBe("number");
    expect(d.condition).toBeTruthy();
  });
});

describe("Crypto Card Mock Data Shape", () => {
  it("has coins array with required fields", () => {
    const reg = getAllRegistrations().find((r) => r.key === "crypto");
    const d = reg!.mockData!;
    expect(Array.isArray(d.coins)).toBe(true);
    const coins = d.coins as Array<Record<string, unknown>>;
    expect(coins.length).toBeGreaterThan(0);
    for (const coin of coins) {
      expect(coin.symbol).toBeTruthy();
      expect(coin.name).toBeTruthy();
      expect(coin.price).toBeTruthy();
      expect(typeof coin.change).toBe("number");
    }
  });
});

describe("News Card Mock Data Shape", () => {
  it("has items array with required fields", () => {
    const reg = getAllRegistrations().find((r) => r.key === "news");
    const d = reg!.mockData!;
    expect(Array.isArray(d.items)).toBe(true);
    const items = d.items as Array<Record<string, unknown>>;
    expect(items.length).toBeGreaterThan(0);
    for (const item of items) {
      expect(item.headline).toBeTruthy();
      expect(item.tag).toBeTruthy();
      expect(item.timeAgo).toBeTruthy();
    }
  });
});

describe("Calendar Card Mock Data Shape", () => {
  it("has events array with required fields", () => {
    const reg = getAllRegistrations().find((r) => r.key === "calendar");
    const d = reg!.mockData!;
    expect(Array.isArray(d.events)).toBe(true);
    const events = d.events as Array<Record<string, unknown>>;
    expect(events.length).toBeGreaterThan(0);
    for (const ev of events) {
      expect(ev.title).toBeTruthy();
      expect(ev.time).toBeTruthy();
    }
  });
});

describe("Map Card Mock Data Shape", () => {
  it("has routes array with required fields", () => {
    const reg = getAllRegistrations().find((r) => r.key === "map");
    const d = reg!.mockData!;
    expect(d.origin).toBeTruthy();
    expect(d.destination).toBeTruthy();
    expect(Array.isArray(d.routes)).toBe(true);
    const routes = d.routes as Array<Record<string, unknown>>;
    expect(routes.length).toBeGreaterThan(0);
    expect(routes.some((r) => r.best === true)).toBe(true);
    for (const r of routes) {
      expect(r.name).toBeTruthy();
      expect(r.time).toBeTruthy();
    }
  });
});

describe("Smart Home Card Mock Data Shape", () => {
  it("has rooms and sensors arrays", () => {
    const reg = getAllRegistrations().find((r) => r.key === "smarthome");
    const d = reg!.mockData!;
    expect(Array.isArray(d.rooms)).toBe(true);
    expect(Array.isArray(d.sensors)).toBe(true);
    const rooms = d.rooms as Array<Record<string, unknown>>;
    expect(rooms.length).toBeGreaterThan(0);
    for (const r of rooms) {
      expect(r.key).toBeTruthy();
      expect(r.label).toBeTruthy();
    }
    const sensors = d.sensors as Array<Record<string, unknown>>;
    for (const s of sensors) {
      expect(s.label).toBeTruthy();
      expect(s.state).toBeTruthy();
    }
  });
});
