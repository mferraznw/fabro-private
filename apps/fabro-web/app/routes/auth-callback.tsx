import { redirect } from "react-router";
import { getAppConfig } from "../lib/config.server";
import { getGitHubOAuth } from "../lib/github.server";
import { getGoogleOAuth } from "../lib/google.server";
import { getSession, commitSession } from "../lib/session.server";
import type { Route } from "./+types/auth-callback";

export async function loader({ request }: Route.LoaderArgs) {
  const url = new URL(request.url);
  const code = url.searchParams.get("code");
  const state = url.searchParams.get("state");

  const cookies = request.headers.get("Cookie") ?? "";
  const stateMatch = cookies.match(/fabro_oauth_state=([^;]+)/);
  const storedState = stateMatch?.[1];

  if (!code || !state || state !== storedState) {
    throw redirect("/auth/login");
  }

  const { provider, allowed_usernames } = getAppConfig().web.auth;
  const session = await getSession(request);

  if (provider === "google") {
    const codeVerifierMatch = cookies.match(/fabro_code_verifier=([^;]+)/);
    const codeVerifier = codeVerifierMatch?.[1];
    if (!codeVerifier) throw redirect("/auth/login");

    const google = getGoogleOAuth();
    if (!google) throw redirect("/auth/login");

    const tokens = await google.validateAuthorizationCode(code, codeVerifier);
    const accessToken = tokens.accessToken();

    const userResp = await fetch("https://www.googleapis.com/oauth2/v2/userinfo", {
      headers: { Authorization: `Bearer ${accessToken}` },
    });
    const googleUser = (await userResp.json()) as {
      id: string;
      email: string;
      name: string;
      picture: string;
    };

    if (allowed_usernames.length > 0 && !allowed_usernames.includes(googleUser.email)) {
      throw redirect("/auth/login?error=unauthorized");
    }

    session.set("userUrl", `google/${googleUser.email}`);
    session.set("login", googleUser.email);
    session.set("name", googleUser.name);
    session.set("email", googleUser.email);
    session.set("avatarUrl", googleUser.picture);
    session.set("accessToken", accessToken);
  } else {
    const github = getGitHubOAuth();
    const tokens = await github.validateAuthorizationCode(code);
    const accessToken = tokens.accessToken();

    const [userResponse, emailsResponse] = await Promise.all([
      fetch("https://api.github.com/user", {
        headers: { Authorization: `Bearer ${accessToken}` },
      }),
      fetch("https://api.github.com/user/emails", {
        headers: { Authorization: `Bearer ${accessToken}` },
      }),
    ]);
    const profile = (await userResponse.json()) as {
      id: number;
      node_id: string;
      login: string;
      name: string | null;
      avatar_url: string;
    };
    const emails = (await emailsResponse.json()) as Array<{
      email: string;
      primary: boolean;
      verified: boolean;
    }>;
    const primaryEmail = emails.find((e) => e.primary && e.verified)?.email ?? "";

    if (allowed_usernames.length > 0 && !allowed_usernames.includes(profile.login)) {
      throw redirect("/auth/login?error=unauthorized");
    }

    session.set("userUrl", `https://github.com/${profile.login}`);
    session.set("githubId", profile.id);
    session.set("githubNodeId", profile.node_id);
    session.set("login", profile.login);
    session.set("name", profile.name ?? profile.login);
    session.set("email", primaryEmail);
    session.set("avatarUrl", profile.avatar_url);
    session.set("accessToken", accessToken);
  }

  const headers = new Headers();
  headers.append("Set-Cookie", await commitSession(session));
  headers.append("Set-Cookie", "fabro_oauth_state=; HttpOnly; Path=/; Max-Age=0");
  headers.append("Set-Cookie", "fabro_code_verifier=; HttpOnly; Path=/; Max-Age=0");

  return redirect("/start", { headers });
}
