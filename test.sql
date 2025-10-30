insert into rooms values(1);
insert into users values(10);
insert into users values(11);

insert into room_members values(1, 10, null, null);
insert into room_members values(1, 11, null, null);
update room_members set next = 11, prev = 11 where user_id = 10;
update room_members set next = 10, prev = 10 where user_id = 11;