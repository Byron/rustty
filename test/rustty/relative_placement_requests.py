"""Ordinary relative Kitty placements: parent choice, validation and bounded chains."""
from placement_requests import case, command, upload


def requests():
    observe = {'op': 'observe'}

    def put(image=1, placement=1, parent=0, parent_placement=0, extra=''):
        return command(f'a=p,i={image},p={placement},P={parent},Q={parent_placement}{extra}'.encode())

    def relative_case(name, operations):
        request, covers = case('relative/' + name, operations)
        return request, covers

    for quiet in (0, 1, 2):
        for name, setup, options in (
                ('missing-image', [], ',P=9,Q=1'),
                ('missing-placement', [upload(image=9)], ',P=9,Q=1'),
                ('missing-fallback', [upload(image=9)], ',P=9'),
                ('explicit-only', [upload(image=9), put(9, 0, extra=',C=1')], ',P=9,Q=1'),
                ('missing-child', [], ',P=9,Q=1'),
                ('virtual-parent', [], ',U=1,P=9,Q=1'),
                ('virtual-missing-child', [], ',U=1,P=9,Q=1')):
            child = 77 if 'missing-child' in name else 1
            yield relative_case(f'errors/{name}/q{quiet}', [upload(), *setup, b'\x1b[3;4H', observe,
                command(f'a=p,i={child},p=3,q={quiet}{options}'.encode()), observe])

    for flags in ('', ',U=1'):
        yield relative_case('missing-identity/' + flags, [upload(),
            command(('a=p,p=1,P=9' + flags).encode()), observe])
    yield relative_case('numbered-child', [upload(image=0, extra=b'I=7'), put(extra=',C=1'),
        command(b'a=p,I=7,p=1,P=1,Q=1'), observe, put(1, 2, 7, 1), observe])
    for flags in ('', ',U=1'):
        yield relative_case('transmit-display/' + flags, [upload(), put(extra=',C=1'),
            command(('a=T,i=2,p=1,P=9,f=32,s=1,v=1'+flags).encode(), b'\0'*4), observe])

    for ids in ((9, 3, 7), (0, 9, 0, 3), (0, 0, 0)):
        setup = [upload(), upload(image=2)]
        for index, placement in enumerate(ids):
            setup.extend((f'\x1b[{index+2};{index+2}H'.encode(), put(1, placement, extra=',C=1')))
        for q in (0, 3, 9, 7, 1):
            yield relative_case(f'parent-choice/{ids}/Q{q}', [*setup, observe,
                put(2, 1, 1, q, ',H=-2,V=3'), observe])

    for q in (0, 1):
        yield relative_case(f'self/Q{q}', [upload(), put(extra=',C=1'), observe,
            put(1, 1, 1, q), observe])
    for length in (2, 3, 8, 9):
        setup = [upload(), put(extra=',C=1')]
        for placement in range(2, length+1):
            setup.append(put(1, placement, 1, placement-1))
        yield relative_case(f'cycle/{length}', [*setup, observe, put(1, 1, 1, length), observe])
        yield relative_case(f'cycle-middle/{length}', [*setup, observe, put(1, 2, 1, length), observe])

    for separate_images in (False, True):
        setup = [upload(), put(extra=',C=1')]
        for placement in range(2, 12):
            image = placement if separate_images else 1
            if separate_images:
                setup.append(upload(image=image))
            setup.extend((put(image, placement, image-1 if separate_images else 1, placement-1,
                              ',H=1,V=-1'), observe))
        yield relative_case(f'depth/separate-images-{separate_images}', setup)

    # Replacing an ancestor can deepen descendants after their own validation.
    setup = [upload(), put(extra=',C=1')]
    for placement in range(2, 10):
        setup.append(put(1, placement, 1, placement-1, ',H=1,V=1'))
    yield relative_case('deepen-ancestor', [*setup, observe, put(1, 20, extra=',C=1'),
        put(1, 1, 1, 20, ',H=1,V=1'), observe, put(1, 21, 1, 9), observe,
        put(1, 1, extra=',C=1'), observe])

    for offsets in (((1, 2), (-3, 4), (5, -6)),
                    ((2**31-1, -(2**31)), (1, -1), (-1, 1)),
                    ((-(2**31), 2**31-1), (-1, 1), (1, -1)),
                    ((-1, 1), (1, -1), (2**31-1, -(2**31)))):
        setup = [upload(), b'\x1b[3;4H', put(extra=',C=1')]
        for placement, (x, y) in enumerate(offsets, start=2):
            setup.append(put(1, placement, 1, placement-1, f',H={x},V={y}'))
        yield relative_case(f'offsets/{offsets}', [*setup, observe,
            {'op': 'resize', 'cols': 6, 'rows': 8}, observe])

    for move in (0, 1):
        yield relative_case(f'cursor/C{move}', [upload(), put(extra=',C=1'), b'\x1b[6;7H', observe,
            put(1, 2, 1, 1, f',C={move},c=3,r=4,H=-2,V=1,X=3,Y=7'), observe])

    for deletion in ('d=i,i=1,p=2', 'd=I,i=1,p=2', 'd=i,i=1,p=1', 'd=I,i=1,p=1'):
        yield relative_case(f'cascade/{deletion}', [upload(), upload(image=2),
            put(extra=',C=1'), put(1, 2, 1, 1), put(2, 1, 1, 2), observe,
            command(('a=d,'+deletion).encode()), observe, put(2, 2, 1, 2), observe])


if __name__ == '__main__':
    import json
    import sys
    json.dump([{'request': request, 'covers': covers} for request, covers in requests()], sys.stdout)
