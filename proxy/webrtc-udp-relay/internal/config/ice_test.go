package config

import "testing"

func TestParseICEServersJSON(t *testing.T) {
	t.Parallel()

	raw := `[
	  {
	    "urls": ["stun:stun.example.com:3478"]
	  },
	  {
	    "urls": ["turn:turn.example.com:3478?transport=udp"],
	    "username": "user",
	    "credential": "pass"
	  }
	]`

	servers, err := ParseICEServersJSON(raw, false)
	if err != nil {
		t.Fatalf("expected success, got %v", err)
	}
	if len(servers) != 2 {
		t.Fatalf("expected 2 servers, got %d", len(servers))
	}

	if got := servers[0].URLs; len(got) != 1 || got[0] != "stun:stun.example.com:3478" {
		t.Fatalf("unexpected stun urls: %#v", got)
	}
	if got := servers[1].Username; got != "user" {
		t.Fatalf("unexpected username: %q", got)
	}
	cred, ok := servers[1].Credential.(string)
	if !ok || cred != "pass" {
		t.Fatalf("unexpected credential: %#v", servers[1].Credential)
	}
}

func TestParseICEServersJSON_SupportsSingleStringURLs(t *testing.T) {
	t.Parallel()

	raw := `[
	  {
	    "urls": "stun:stun.example.com:3478"
	  }
	]`

	servers, err := ParseICEServersJSON(raw, false)
	if err != nil {
		t.Fatalf("expected success, got %v", err)
	}
	if len(servers) != 1 {
		t.Fatalf("expected 1 server, got %d", len(servers))
	}
	if got := servers[0].URLs; len(got) != 1 || got[0] != "stun:stun.example.com:3478" {
		t.Fatalf("unexpected urls: %#v", got)
	}
}

func TestParseICEServersJSON_RejectsTURNWithoutCreds(t *testing.T) {
	t.Parallel()

	raw := `[
	  {
	    "urls": ["turn:turn.example.com:3478?transport=udp"]
	  }
	]`

	_, err := ParseICEServersJSON(raw, false)
	if err == nil {
		t.Fatal("expected error")
	}
}

func TestParseICEServersJSON_AllowsTURNWithoutCredsWhenEnabled(t *testing.T) {
	t.Parallel()

	raw := `[
	  {
	    "urls": ["turn:turn.example.com:3478?transport=udp"]
	  }
	]`

	servers, err := ParseICEServersJSON(raw, true)
	if err != nil {
		t.Fatalf("expected success, got %v", err)
	}
	if len(servers) != 1 {
		t.Fatalf("expected 1 server, got %d", len(servers))
	}
	if servers[0].Username != "" || servers[0].Credential != nil {
		t.Fatalf("expected TURN server creds to be empty: %#v", servers[0])
	}
}

func TestParseICEServersFromConvenienceEnv(t *testing.T) {
	t.Parallel()

	servers, err := ParseICEServersFromConvenienceEnv(
		"stun:stun.example.com:3478",
		"turn:turn.example.com:3478?transport=udp",
		"user",
		"pass",
		false,
	)
	if err != nil {
		t.Fatalf("expected success, got %v", err)
	}
	if len(servers) != 2 {
		t.Fatalf("expected 2 servers, got %d", len(servers))
	}
	if servers[0].Username != "" || servers[0].Credential != nil {
		t.Fatalf("stun server should not have creds: %#v", servers[0])
	}
	if servers[1].Username != "user" {
		t.Fatalf("unexpected turn username: %q", servers[1].Username)
	}
	if servers[1].Credential.(string) != "pass" {
		t.Fatalf("unexpected turn credential: %#v", servers[1].Credential)
	}
}

func TestParseICEServersFromConvenienceEnv_AllowsTURNWithoutCredsWhenEnabled(t *testing.T) {
	t.Parallel()

	servers, err := ParseICEServersFromConvenienceEnv(
		"",
		"turn:turn.example.com:3478?transport=udp",
		"",
		"",
		true,
	)
	if err != nil {
		t.Fatalf("expected success, got %v", err)
	}
	if len(servers) != 1 {
		t.Fatalf("expected 1 server, got %d", len(servers))
	}
	if servers[0].Username != "" || servers[0].Credential != nil {
		t.Fatalf("expected TURN server creds to be empty: %#v", servers[0])
	}
}
