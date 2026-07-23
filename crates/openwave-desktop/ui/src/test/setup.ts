// Shared vitest setup: registers Testing Library's jest-dom matchers
// (toBeDisabled, toHaveTextContent, …) for every test file. Harmless in the
// node environment; meaningful in files that opt into jsdom.
import "@testing-library/jest-dom/vitest";
