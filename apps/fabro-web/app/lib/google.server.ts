import { Google } from "arctic";
import { getAppConfig } from "./config.server";

let googleOAuth: Google | null = null;

export function getGoogleOAuth(): Google | null {
  if (googleOAuth) return googleOAuth;

  const config = getAppConfig();
  const clientId = config.web.auth.google_client_id;
  const clientSecret = process.env.GOOGLE_CLIENT_SECRET;

  if (!clientId || !clientSecret) return null;

  const redirectUri = `${config.web.url}/auth/callback`;
  googleOAuth = new Google(clientId, clientSecret, redirectUri);
  return googleOAuth;
}

export function isGoogleOAuthConfigured(): boolean {
  return getGoogleOAuth() !== null;
}
