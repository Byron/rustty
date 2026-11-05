use rustty_vt::{Effect, Terminal};

fn query(t: &mut Terminal, body: &str) -> Vec<Effect> {
    t.feed(format!("\x1b_G{body}\x1b\\").as_bytes())
}

fn reply(keys: &str) -> Vec<Effect> {
    vec![Effect::Write(
        format!("\x1b_Gi=31{keys};OK\x1b\\").into_bytes(),
    )]
}

#[test]
fn reports_only_implemented_actions_keys_and_values() {
    for tmux in [false, true] {
        let mut t = Terminal::new(10, 3, 100);
        t.tmux_graphics = tmux;
        for (body, keys) in [
            ("e=T,C=1,U=1,N=1,f=16,t=f", ",e=T,C=1,U=1,f=16"),
            ("e=p,U=1,N=1", ",e=p,U=1"),
            ("e=T,U=0,N=1", ",e=T"),
            ("e=T,U=2,N=1", ",e=T"),
            ("e=p,U=2,N=1", ",e=p"),
            ("e=t,U=1,N=1", ",e=t"),
            ("e=q,U=1,N=1", ",e=q"),
            ("e=f,U=1,N=1", ",e=f"),
            ("e=t,f=8", ",e=t,f=8"),
            ("e=q,f=16", ",e=q,f=16"),
            ("e=f,f=8", ",e=f"),
            ("e=f,f=16", ",e=f"),
            ("e=f,r=1,X=1,f=24,o=z", ",e=f,X=1,f=24,o=z,r=1"),
            ("e=a,s=3,v=2,z=-1", ",e=a,s=3,v=2,z=-1"),
            ("e=a,s=4,C=1", ",e=a"),
            ("e=q,f=32,s=10001,v=10000", ",e=q,f=32,v=10000"),
            ("e=c,r=2,c=1,C=2", ",e=c,c=1,r=2"),
            ("e=x,C=1", ""),
            ("e=340,C=1", ""),
            ("e=p,p=2,C=0,Z=1", ",e=p,p=2"),
        ] {
            assert_eq!(
                query(&mut t, &format!("a=q,i=31,{body};ignored!")),
                reply(keys),
                "{body}"
            );
        }
        let keys = if tmux {
            ",e=d,p=2"
        } else {
            ",e=d,d=z,p=2,z=-2"
        };
        assert_eq!(query(&mut t, "a=q,i=31,e=d,d=z,p=2,z=-2"), reply(keys));
        for body in [
            "a=q,e=T",
            "a=q,i=31,I=2,e=T",
            "a=T,i=31,e=T",
            "a=q,i=31,e=T,q=1",
            "a=q,i=31,e=T,q=2",
            "a=q,i=31,e=p,U=-1,N=1",
        ] {
            assert!(query(&mut t, body).is_empty(), "{body}");
        }
        t.set_graphics_limit(0);
        for action in ["T", "p"] {
            assert_eq!(
                query(&mut t, &format!("a=q,i=31,e={action},C=1,U=1,N=1,f=8")),
                reply("")
            );
        }
    }
}

#[test]
fn discovery_preserves_images_pending_chunks_and_terminal_state_at_every_split() {
    for tmux in [false, true] {
        let input = b"\x1b_Ga=q,i=31,e=T,C=1,U=1,N=1,f=8,m=1;invalid-payload\x1b\\";
        for split in 0..=input.len() {
            let mut t = Terminal::new(10, 3, 100);
            t.tmux_graphics = tmux;
            query(&mut t, "a=T,i=2,p=9,U=1,c=2,r=1,f=32,s=1,v=1,q=2;AQID/w==");
            assert!(
                t.graphics()
                    .placeholder_target(2, 9)
                    .unwrap()
                    .virtual_placement
            );
            query(&mut t, "a=t,i=1,s=1,v=1,f=24,m=1;AQID");
            let snapshot = rustty_vt::snapshot::encode_to_vec(&t).unwrap();
            let generation = t.generation;
            let mut effects = t.feed(&input[..split]);
            effects.extend(t.feed(&input[split..]));
            assert_eq!(effects, reply(",e=T,C=1,U=1,f=8"));
            assert_eq!(t.generation, generation);
            assert_eq!(rustty_vt::snapshot::encode_to_vec(&t).unwrap(), snapshot);
            query(&mut t, "m=0;");
            assert_eq!(t.graphics().images[&1].pixels.as_slice(), &[1, 2, 3, 255]);
        }
    }
}

#[test]
fn placeholder_feature_replies_precede_device_attributes() {
    #[derive(Default)]
    struct Host(Vec<Effect>);
    impl rustty_vt::EffectHandler for Host {
        fn effect(&mut self, effect: Effect) {
            self.0.push(effect);
        }
        fn device_attributes(&mut self) -> Option<rustty_vt::query::DeviceAttributes> {
            Some(Default::default())
        }
    }
    let input = b"\x1b_Ga=q,i=31,e=T,U=1,N=1\x1b\\\x1b_Ga=q,i=32,e=p,U=1\x1b\\\x1b[c";
    for split in 0..=input.len() {
        let mut terminal = Terminal::new(10, 3, 100);
        let mut host = Host::default();
        terminal.feed_with_handler(&input[..split], &mut host);
        terminal.feed_with_handler(&input[split..], &mut host);
        assert_eq!(
            host.0,
            [
                Effect::Write(b"\x1b_Gi=31,e=T,U=1;OK\x1b\\".to_vec()),
                Effect::Write(b"\x1b_Gi=32,e=p,U=1;OK\x1b\\".to_vec()),
                Effect::Write(
                    rustty_vt::query::DeviceAttributes::default()
                        .encode(rustty_vt::query::AttributeKind::Primary)
                ),
            ]
        );
    }
}
