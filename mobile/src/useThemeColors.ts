import { useColorScheme } from "react-native";
import { schemeOf, theme } from "./theme";

export function useThemeColors() {
  return theme[schemeOf(useColorScheme())];
}
