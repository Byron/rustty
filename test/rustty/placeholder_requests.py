"""Native Kitty Unicode placeholder runs, IDs and stable placement targets."""
from pathlib import Path
import re
from placement_requests import case, command, upload

# Exercise Rust's table with the native protocol's reference codepoints.
_source = (Path(__file__).resolve().parents[2] / 'src/terminal/kitty/graphics_unicode.zig').read_text()
_table = _source.split('const diacritics: []const u21 = &.{', 1)[1].split('};', 1)[0]
DIACRITICS = [chr(int(cp, 16)) for cp in re.findall(r'0x([0-9A-Fa-f]+)', _table)]
PLACEHOLDER = '\U0010eeee'


def placeholder(*indices):
    return (PLACEHOLDER + ''.join(DIACRITICS[i] if isinstance(i, int) else i for i in indices)).encode()


def style(image=1, placement=None):
    result = f'\x1b[38;2;{image >> 16 & 255};{image >> 8 & 255};{image & 255}m'
    if placement is not None:
        result += f'\x1b[58;2;{placement >> 16 & 255};{placement >> 8 & 255};{placement & 255}m'
    return result.encode()


def put(placement=1, image=1, extra=',U=1,c=3,r=2'):
    return command(f'a=p,i={image},p={placement},C=1{extra}'.encode())


def placeholder_case(name, operations, cell=(8,16)):
    request, covers = case('placeholder/' + name, [b'\x1b[?2027h', *operations], cell=cell)
    return request, covers


def requests():
    observe = {'op': 'observe'}
    for name, content in (
            ('implicit', placeholder()*3),
            ('explicit', placeholder(0,0)+placeholder(0,1)+placeholder(0,2)),
            ('inherit', placeholder(1,2)+placeholder()*2),
            ('high-inherit', placeholder(0,0,1)+placeholder()+placeholder(0,2)),
            ('high-zero-break', placeholder(0,0)+placeholder(0,1,0)+placeholder()),
            ('high-change', placeholder(0,0,1)+placeholder(0,1,2)+placeholder()),
            ('row-change', placeholder(0,0)+placeholder(1,1)+placeholder()),
            ('column-change', placeholder(0,0)+placeholder(0,2)+placeholder()),
            ('invalid-row', placeholder('\u0300',1)+placeholder()),
            ('invalid-column', placeholder(1,'\u0300')+placeholder()),
            ('invalid-high', placeholder(0,0,'\u0300')+placeholder()),
            ('invalid-high-index', placeholder(0,0,256)+placeholder()),
            ('additional-marks', placeholder(0,0,1,2,3)+placeholder()),
            ('non-placeholder', placeholder()+b'A'+placeholder()+b' '+placeholder()),
            ('rows', placeholder()+b'\r\n'+placeholder()),
    ):
        yield placeholder_case('runs/'+name, [style(), content])

    # All valid row/column/high marks, including marks above the 8-bit high-ID range.
    for index in range(len(DIACRITICS)):
        yield placeholder_case(f'diacritics/{index}', [style(),placeholder(index,index,index)])

    for name, colors in (
            ('indexed', b'\x1b[38;5;42m\x1b[58;5;21m'),
            ('rgb', style(0x123456,0x654321)),
            ('default', b'\x1b[39m\x1b[59m'),
            ('zero-index-default', b'\x1b[38;5;1m\x1b[58;5;0m'),
    ):
        yield placeholder_case('colors/'+name,[colors,placeholder(),b'\x1b[59m',placeholder()])

    for ids in ((9,3,7),(0,9,0,3),(0,0,0)):
        setup = [upload()]
        for pid in ids:
            setup.append(put(pid))
        for explicit in (0,3,9,7,1):
            yield placeholder_case(f'target/{ids}/{explicit}',[*setup,style(1,explicit),placeholder()])

    for kind, extra in (('pin',',c=2,r=1'),('relative',',c=2,r=1,P=1,Q=9')):
        yield placeholder_case('target/'+kind,[upload(),put(9),put(2,extra=extra),
                              style(1,2),placeholder(0,0)+placeholder(),observe])
    yield placeholder_case('target/missing-image',[style(2,3),placeholder()])
    yield placeholder_case('target/missing-placement',[upload(),style(1,3),placeholder()])
    yield placeholder_case('target/nonvirtual-fallback',[upload(),put(2,extra=',c=2,r=1'),
                                                       style(1,0),placeholder()])
    yield placeholder_case('target/replacement',[upload(),put(9),put(3),style(1,0),placeholder(),observe,
                          put(3,extra=',c=1,r=1'),observe,command(b'a=d,d=i,i=1,p=9'),observe])

    for name, operation in (
            ('scroll',b'\r\nL'*12),('erase',b'\x1b[2J'),
            ('reflow',{'op':'resize','cols':6,'rows':8}),
            ('reset',{'op':'terminal_reset'}),
    ):
        yield placeholder_case('lifecycle/'+name,[upload(),put(),style(),b'\x1b[2;3H',
                              placeholder(0,0)+placeholder(),observe,operation,observe])
    for mode in (47,1049):
        yield placeholder_case(f'screens/{mode}',[upload(),put(),style(),placeholder(),observe,
            f'\x1b[?{mode}h'.encode(),upload(4,3),put(),style(),placeholder(1,0),observe,
            f'\x1b[?{mode}l'.encode(),observe])


if __name__ == '__main__':
    import json,sys
    json.dump([{'request': request, 'covers': covers} for request,covers in requests()],sys.stdout)
