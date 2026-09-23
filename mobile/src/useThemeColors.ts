import { vars } from "nativewind";
import { useColorScheme } from "react-native";
import { cssVarsFor, schemeOf, theme } from "./theme";

export function useThemeColors() {
  return theme[schemeOf(useColorScheme())];
}

export function useThemeCssVars() {
  return vars(cssVarsFor(useThemeColors()));
}
