import { Platform } from "react-native";
import * as SecureStore from "expo-secure-store";
import { TokenStore } from "../lib/tokenStore";
import { fetchTokenHttp } from "../lib/gateway";
import { storageForOs, type SecureStorage } from "../lib/storage";

const expoStorage: SecureStorage = {
  getItem: (key) => SecureStore.getItemAsync(key),
  setItem: (key, value) => SecureStore.setItemAsync(key, value),
  deleteItem: (key) => SecureStore.deleteItemAsync(key),
};

export const tokenStore = new TokenStore(
  storageForOs(Platform.OS, expoStorage),
  fetchTokenHttp(),
);
