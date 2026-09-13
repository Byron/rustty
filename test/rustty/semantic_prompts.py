"""Native OSC 133 actions, options, prompt rows and snapshot continuation."""


def requests():
    fixtures=[]

    def packet(body, end=b'\x07'):
        return b'\x1b]133;'+body+end

    def add(name, initial, body, end=b'\x07', tail=b'X\r\nY', kind='terminal'):
        operations=[{'op':'write','data':initial.hex()}, {'op':'observe'},
                    {'op':'write','data':packet(body,end).hex()}, {'op':'observe'}]
        request={'id':'protocol/semantic/'+name,'kind':kind,'cols':8,'rows':5,'operations':operations,'observe_semantic':True}
        if kind=='snapshot':
            request['after']=[{'op':'write','data':tail.hex()}]
        else:
            operations.extend([{'op':'write','data':tail.hex()},{'op':'observe'}])
        fixtures.append({'request':request,'covers':['terminal.cells','terminal.cursor','parser.events']})

    initials={'origin':b'', 'text':b'one\r\ntwo', 'left':b'one\r\n',
              'wrap':b'abcdefgh','bottom':b'\x1b[5;4H',
              'margin':b'\x1b[?69h\x1b[2;6s\x1b[2;4r\x1b[3;4H',
              'origin-margin':b'\x1b[?69h\x1b[2;6s\x1b[2;4r\x1b[?6h\x1b[2;3H'}
    for action in b'LANPBICD':
        for name,initial in initials.items():
            for end in [b'\x07',b'\x1b\\']:
                add(f'position/{chr(action)}/{name}/{end.hex()}',initial,bytes([action]),end)
        for suffix in [b'',b';',b';aid=x',b';k=i',b';k=r',b';k=c',b';k=s',b';k=',b';k=unknown',
                       b';redraw=0',b';redraw=1',b';redraw=last',b';redraw=x',
                       b';click_events=1;cl=line',b';click_events=0;cl=word',b';cl=v',b';cl=w',b';special_key=1',
                       b';0',b';-1',b';2147483647',b';-2147483648',b';2147483648',b';cmdline=echo hello',
                       b';cmdline_url=echo%20hello',b';6;7;\xff']:
            add(f'options/{chr(action)}/{suffix.hex()}',b'one\r\ntwo',bytes([action])+suffix)
        for previous in b'PBICD':
            add(f'transition/{chr(previous)}/{chr(action)}',packet(bytes([previous]))+b'prior',bytes([action]))
        for end in [b'\x18',b'\x1a',b'\x1b[0m',b'\x1bX']:
            add(f'termination/{chr(action)}/{end.hex()}',b'prior',bytes([action]),end)
        add(f'snapshot/{chr(action)}',b'prior',bytes([action]),kind='snapshot')
    for action in b'ANP':
        for length in [2047,2048,2049,4096]:
            add(f'capture/{chr(action)}/{length}',b'prior',bytes([action])+b';'+b'x'*(length-2))
    for body in [b'',b';A',b' A',b'a',b'\xff',b'L;',b'L;aid=x',b'AA',b'Bextra',b'Pextra',b'Cextra',b'Dextra',b'Nextra',b'Iextra']:
        add('invalid/'+body.hex(),b'prior',body)

    # Begin with nondefault state so missing/invalid options cannot accidentally
    # pass by reproducing default metadata. Native option lookup stops at the first
    # matching key, even if its value is invalid.
    metadata = packet(b'A;k=s;redraw=last;click_events=2')+b'prompt'
    for action in b'ANP':
        for option in [b'k=i;k=s',b'k=s;k=i',b'k=bad;k=s',b'k=;k=s',
                       b'redraw=0;redraw=1',b'redraw=1;redraw=0',b'redraw=bad;redraw=0',
                       b'redraw=;redraw=0',b'cl=line',b'cl=m',b'cl=v',b'cl=w',
                       b'cl=w;cl=line',b'cl=bad;cl=w',b'cl=;cl=w',
                       b'click_events=1',b'click_events=2',b'click_events=0',
                       b'click_events=1;cl=w',b'cl=w;click_events=2',
                       b'click_events=0;cl=w',b'click_events=bad;cl=w',
                       b'click_events=1;click_events=2',b'click_events=bad;click_events=1;cl=w',
                       b'unknown;cl=w',b';cl=w',b'unknown=anything;redraw=0;cl=w']:
            add(f'metadata/{chr(action)}/{option.hex()}',metadata,bytes([action])+b';'+option)
        for option in [b'redraw=last;cl=w;k=s',b'redraw=0;click_events=1;k=r',b'click_events=2;k=c']:
            add(f'snapshot-options/{chr(action)}/{option.hex()}',metadata,bytes([action])+b';'+option,
                tail=b'X\r\nY'+packet(b'I')+b'abcdefghijk\r\nZ',kind='snapshot')

    for kind in b'ircs':
        for action in b'PBICD':
            for transition_name, transition in [('same-row',b''),('lf',b'\r\n'),('wrap',b'abcdefgh')]:
                add(f'row-kind/{chr(kind)}/{chr(action)}/{transition_name}',
                    packet(b'P;k='+bytes([kind]))+transition,bytes([action]),tail=b'X\r\nY')

    for action in b'BI':
        for line_name,line in [('lf',b'\n'),('crlf',b'\r\n'),('index',b'\x1bD'),
                              ('nel',b'\x1bE'),('wrap',b'abcdefghi'),('scroll',b'\x1b[5;1H\n')]:
            add(f'eol/{chr(action)}/{line_name}',packet(b'P')+b'>',bytes([action]),tail=b'x'+line+b'Y')
        for mode in [47,1047,1049]:
            switch=f'\x1b[?{mode}h'.encode()
            back=f'\x1b[?{mode}l'.encode()
            add(f'alternate/{chr(action)}/{mode}',metadata,bytes([action]),
                tail=switch+b'X\r\nY'+packet(b'A;cl=w;redraw=0')+b'alt'+back+b'Z')
            add(f'alternate-snapshot/{chr(action)}/{mode}',metadata+packet(bytes([action]))+switch,
                b'A;cl=w;redraw=last;k=s',tail=b'X\r\nY'+back+b'Z',kind='snapshot')

    for name,reset in [('direct',{'op':'terminal_reset'}),('ris',{'op':'reset'})]:
        request={'id':f'protocol/semantic/reset/{name}','cols':8,'rows':5,'observe_semantic':True,
                 'operations':[{'op':'write','data':(metadata+packet(b'I')).hex()},{'op':'observe'},reset,
                               {'op':'observe'},{'op':'write','data':b'X\r\nY'.hex()}]}
        fixtures.append({'request':request,'covers':['terminal.cells','terminal.cursor','terminal.reset']})

    marked_rows=b'\r\n'.join(packet(b'P;k='+kind)+b'line'+packet(b'B')+b'>>' for kind in [b'i',b'r',b'c',b's',b'i'])
    for column in [1,4,8]:
        for erase in [b'J',b'1J',b'2J',b'K',b'1K',b'2K',b'X',b'8X',b'0X']:
            operations=[{'op':'write','data':(marked_rows+f'\x1b[3;{column}H'.encode()).hex()},
                        {'op':'observe'},{'op':'write','data':(b'\x1b['+erase).hex()},{'op':'observe'}]
            fixtures.append({'request':{'id':f'protocol/semantic/erase/{column}/{erase.decode()}',
                                       'cols':8,'rows':5,'observe_semantic':True,'operations':operations},
                             'covers':['terminal.cells','terminal.cursor']})

    for protection,start,end in [('dec',b'\x1b[1"q',b'\x1b[0"q'),('iso',b'\x1bV',b'\x1bW')]:
        marked=b'\r\n'.join(packet(b'P;k='+kind)+start+b'one'+end+b'two' for kind in [b'i',b'c',b's',b'r'])
        for erase in [b'J',b'1J',b'2J',b'?J',b'?1J',b'?2J',b'K',b'?2K',b'8X']:
            request={'id':f'protocol/semantic/protected/{protection}/{erase.decode()}',
                     'cols':8,'rows':5,'observe_semantic':True,
                     'operations':[{'op':'write','data':(marked+packet(b'C')+b'\x1b[2;4H').hex()},
                                   {'op':'observe'},{'op':'write','data':(b'\x1b['+erase).hex()},{'op':'observe'}]}
            fixtures.append({'request':request,'covers':['terminal.cells','terminal.cursor']})

        # A protected row keeps its attributes even if every unprotected cell
        # is erased. Reflow needs both sides of the soft-wrap relationship.
        wrapped=packet(b'P;k=s')+start+b'ab'+end+b'cdefghijklmno'+packet(b'C')+b'\x1b[3;1Houtput'
        for erase in [b'J',b'1J',b'2J',b'?J',b'?1J',b'?2J']:
            request={'id':f'protocol/semantic/protected-wrap/{protection}/{erase.decode()}',
                     'cols':8,'rows':5,'observe_semantic':True,
                     'operations':[{'op':'write','data':(wrapped+b'\x1b[2;4H').hex()},
                                   {'op':'observe'},{'op':'write','data':(b'\x1b['+erase).hex()},{'op':'observe'}]}
            fixtures.append({'request':request,'covers':['terminal.cells','terminal.cursor']})

    yield from ((fixture["request"], fixture["covers"]) for fixture in fixtures)
